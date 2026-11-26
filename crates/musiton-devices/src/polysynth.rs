//! Polysynth: 2 oscillators + noise → state-variable filter with envelope → amp ADSR,
//! LFO on pitch and cutoff, glide, unison (up to 7 detuned oscillators per voice, spread
//! across the stereo field), analog-style drift, a drive stage (anti-aliased distortion) and
//! 16 voices with voice stealing. The low end stays mono however wide the unison is.
//!
//! Just intonation (optional): held notes are retuned live to whole-number ratios above
//! the lowest held note (fifth 3:2, major third 5:4, seventh 7:4 …), and osc 2's interval
//! too. Distorted, such chords make difference tones that land exactly on the overtone
//! series instead of beating: the clean but "raw" sound of the owner's reference.

use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_dsp::env::Adsr;
use musiton_dsp::filter::{FilterMode, Svf};
use musiton_dsp::osc::{BlepOsc, Noise, Wave};
use musiton_dsp::shaper::{Curve, Shaper};
use musiton_dsp::smooth::LinearRamp;

use crate::controllers::{Change, Controllers};
use musiton_engine::node::{AudioIo, AudioNode, Event, EventKind, ProcessContext};

/// Number of voices.
pub const VOICES: usize = 16;
/// Filter coefficients are recomputed every this many samples.
const FILTER_UPDATE: usize = 8;
/// Output headroom.
const HEADROOM: f32 = 0.25;
/// Vibrato depth (semitones) at full mod wheel.
const MOD_WHEEL_VIBRATO: f32 = 0.5;
/// Most unison oscillators per voice and oscillator.
pub const MAX_UNISON: usize = 7;

/// Below this frequency (Hz) the output is mono (side signal high-passed).
const MONO_BASS_HZ: f32 = 150.0;
/// Most drive (dB) at the top of the knob.
const MAX_DRIVE_DB: f32 = 42.0;
/// Pitch wander (semitones) and fixed per-copy offset (semitones) at full drift.
const DRIFT_WANDER: f32 = 0.10;
const DRIFT_SCATTER: f32 = 0.05;
/// Drift wander: a new target every this many samples, approached with this time (s).
const DRIFT_STEP: u64 = 4_096;
const DRIFT_TIME: f32 = 0.4;

const WAVES: &[&str] = &["Saw", "Square", "Triangle", "Sine"];
const CURVES: &[&str] = &["Soft", "Hard", "Asym", "Fold"];
/// Time (s) held notes take to slide into a new just tuning when the chord changes.
const RETUNE_TIME: f32 = 0.03;

/// Just ratios for the 12 intervals above a root (overtone-series flavoured: the minor
/// seventh is the natural 7:4, the tritone 7:5).
const JUST_RATIOS: [f32; 12] = [
    1.0,
    16.0 / 15.0,
    9.0 / 8.0,
    6.0 / 5.0,
    5.0 / 4.0,
    4.0 / 3.0,
    7.0 / 5.0,
    3.0 / 2.0,
    8.0 / 5.0,
    5.0 / 3.0,
    7.0 / 4.0,
    15.0 / 8.0,
];

/// Correction (semitones) that turns the equal-tempered interval `semis` into its just
/// version, e.g. +7 → +0.02 (a pure fifth is 2 cents wider).
fn just_offset(semis: i32) -> f32 {
    let class = semis.rem_euclid(12);
    let ratio = JUST_RATIOS.get(class as usize).copied().unwrap_or(1.0);
    12.0 * ratio.log2() - class as f32
}
const MODES: &[&str] = &["Low-pass", "High-pass", "Band-pass"];

/// Parameter ids.
#[allow(missing_docs)]
pub mod id {
    pub const OSC1_WAVE: u64 = 0;
    pub const OSC1_LEVEL: u64 = 1;
    pub const OSC2_WAVE: u64 = 2;
    pub const OSC2_LEVEL: u64 = 3;
    pub const OSC2_SEMI: u64 = 4;
    pub const OSC2_DETUNE: u64 = 5;
    pub const NOISE: u64 = 6;
    pub const FILTER_MODE: u64 = 7;
    pub const CUTOFF: u64 = 8;
    pub const RESONANCE: u64 = 9;
    pub const FILTER_ENV: u64 = 10;
    pub const AMP_A: u64 = 11;
    pub const AMP_D: u64 = 12;
    pub const AMP_S: u64 = 13;
    pub const AMP_R: u64 = 14;
    pub const FILT_A: u64 = 15;
    pub const FILT_D: u64 = 16;
    pub const FILT_S: u64 = 17;
    pub const FILT_R: u64 = 18;
    pub const LFO_RATE: u64 = 19;
    pub const LFO_PITCH: u64 = 20;
    pub const LFO_CUTOFF: u64 = 21;
    pub const GLIDE: u64 = 22;
    pub const VOLUME: u64 = 23;
    pub const VELOCITY: u64 = 24;
    pub const UNISON: u64 = 25;
    pub const UNISON_DETUNE: u64 = 26;
    pub const UNISON_SPREAD: u64 = 27;
    pub const DRIVE: u64 = 28;
    pub const DRIVE_TYPE: u64 = 29;
    pub const DRIFT: u64 = 30;
    pub const JUST: u64 = 31;
}

/// All parameters (index = id).
pub const PARAMS: [ParamInfo; 32] = [
    ParamInfo::choice(id::OSC1_WAVE, "Osc 1", WAVES, 0),
    ParamInfo::new(id::OSC1_LEVEL, "Osc 1 level", 0.0, 1.0, 0.8),
    ParamInfo::choice(id::OSC2_WAVE, "Osc 2", WAVES, 1),
    ParamInfo::new(id::OSC2_LEVEL, "Osc 2 level", 0.0, 1.0, 0.0),
    ParamInfo::new(id::OSC2_SEMI, "Osc 2 pitch", -24.0, 24.0, 0.0)
        .int()
        .unit("st"),
    ParamInfo::new(id::OSC2_DETUNE, "Osc 2 detune", -50.0, 50.0, 7.0).unit("ct"),
    ParamInfo::new(id::NOISE, "Noise", 0.0, 1.0, 0.0),
    ParamInfo::choice(id::FILTER_MODE, "Filter", MODES, 0),
    ParamInfo::new(id::CUTOFF, "Cutoff", 20.0, 20_000.0, 4_000.0)
        .log()
        .unit("Hz"),
    ParamInfo::new(id::RESONANCE, "Resonance", 0.0, 1.0, 0.2),
    ParamInfo::new(id::FILTER_ENV, "Filter env", -1.0, 1.0, 0.3),
    ParamInfo::new(id::AMP_A, "Attack", 0.001, 5.0, 0.005)
        .log()
        .unit("s"),
    ParamInfo::new(id::AMP_D, "Decay", 0.001, 5.0, 0.3)
        .log()
        .unit("s"),
    ParamInfo::new(id::AMP_S, "Sustain", 0.0, 1.0, 0.7),
    ParamInfo::new(id::AMP_R, "Release", 0.001, 10.0, 0.3)
        .log()
        .unit("s"),
    ParamInfo::new(id::FILT_A, "F attack", 0.001, 5.0, 0.005)
        .log()
        .unit("s"),
    ParamInfo::new(id::FILT_D, "F decay", 0.001, 5.0, 0.4)
        .log()
        .unit("s"),
    ParamInfo::new(id::FILT_S, "F sustain", 0.0, 1.0, 0.2),
    ParamInfo::new(id::FILT_R, "F release", 0.001, 10.0, 0.3)
        .log()
        .unit("s"),
    ParamInfo::new(id::LFO_RATE, "LFO rate", 0.05, 20.0, 5.0)
        .log()
        .unit("Hz"),
    ParamInfo::new(id::LFO_PITCH, "LFO to pitch", 0.0, 2.0, 0.0).unit("st"),
    ParamInfo::new(id::LFO_CUTOFF, "LFO to cutoff", 0.0, 1.0, 0.0),
    ParamInfo::new(id::GLIDE, "Glide", 0.0, 2.0, 0.0).unit("s"),
    ParamInfo::new(id::VOLUME, "Volume", 0.0, 1.0, 0.7),
    ParamInfo::new(id::VELOCITY, "Velocity", 0.0, 1.0, 0.7),
    ParamInfo::new(id::UNISON, "Unison", 1.0, MAX_UNISON as f32, 1.0).int(),
    ParamInfo::new(id::UNISON_DETUNE, "Unison detune", 0.0, 100.0, 20.0).unit("ct"),
    ParamInfo::new(id::UNISON_SPREAD, "Unison width", 0.0, 1.0, 0.5),
    ParamInfo::new(id::DRIVE, "Drive", 0.0, 1.0, 0.0),
    ParamInfo::choice(id::DRIVE_TYPE, "Drive type", CURVES, 0),
    ParamInfo::new(id::DRIFT, "Analog drift", 0.0, 1.0, 0.3),
    ParamInfo::toggle(id::JUST, "Just intonation", false),
];

#[derive(Debug, Clone)]
struct Voice {
    key: Option<u8>,
    osc1: [BlepOsc; MAX_UNISON],
    osc2: [BlepOsc; MAX_UNISON],
    /// Left (or mono) filter.
    filter: Svf,
    /// Right filter, used while unison is spread across the stereo field.
    filter_r: Svf,
    amp: Adsr,
    fenv: Adsr,
    pitch: f32,
    target: f32,
    vel_gain: f32,
    age: u64,
    /// The key was released while the sustain pedal was down.
    pedal_held: bool,
    /// Random state (xorshift) for drift and start phases.
    rng: u32,
    /// Slow pitch wander (−1 … 1) and where it is heading.
    drift: f32,
    drift_to: f32,
    /// Fixed random pitch offset per unison copy (−1 … 1), new on every note.
    scatter: [f32; MAX_UNISON],
    /// Just-intonation correction (semitones): wanted and current (it slides).
    just: f32,
    just_now: f32,
}

impl Voice {
    /// Voice with its own random sequence.
    fn seeded(seed: u32) -> Self {
        Self {
            rng: seed.wrapping_mul(0x9E37_79B9) | 1,
            ..Self::default()
        }
    }

    /// Next random value in −1 … 1.
    fn random(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

impl Default for Voice {
    fn default() -> Self {
        // Unison oscillators start at spread-out phases so a chord does not start with
        // every copy in phase (a loud click-like attack). The first one starts at 0.
        let phases = std::array::from_fn(|i| BlepOsc::with_phase(i as f32 * 0.618_034));
        Self {
            key: None,
            osc1: phases.clone(),
            osc2: phases,
            filter: Svf::default(),
            filter_r: Svf::default(),
            amp: Adsr::default(),
            fenv: Adsr::default(),
            pitch: 0.0,
            target: 0.0,
            vel_gain: 0.0,
            age: 0,
            pedal_held: false,
            rng: 1,
            drift: 0.0,
            drift_to: 0.0,
            scatter: [0.0; MAX_UNISON],
            just: 0.0,
            just_now: 0.0,
        }
    }
}

/// Per-copy pitch offset (semitones) and left/right gains for unison.
#[derive(Debug, Clone, Copy)]
struct UnisonCopy {
    offset: f32,
    left: f32,
    right: f32,
}

/// Pitch offsets and pans of `count` unison copies (`detune` in cents, `width` 0 … 1),
/// scaled so the level stays about the same whatever the count.
fn unison_copies(count: usize, detune: f32, width: f32) -> [UnisonCopy; MAX_UNISON] {
    let count = count.clamp(1, MAX_UNISON);
    let gain = 1.0 / (count as f32).sqrt();
    std::array::from_fn(|i| {
        let pos = if count > 1 {
            2.0 * i as f32 / (count - 1) as f32 - 1.0
        } else {
            0.0
        };
        let pan = pos * width;
        // Inner copies sit closer to the centre than the outer ones (an even spread
        // beats in a regular, "mechanical" pattern).
        let bend = pos * (0.35 + 0.65 * pos * pos);
        UnisonCopy {
            offset: bend * detune / 100.0,
            left: gain * (1.0 - pan.max(0.0)),
            right: gain * (1.0 + pan.min(0.0)),
        }
    })
}

/// The polysynth node (no inputs, stereo out).
#[derive(Debug)]
pub struct Polysynth {
    values: [f32; PARAMS.len()],
    voices: Vec<Voice>,
    sample_rate: f32,
    clock: u64,
    lfo_phase: f32,
    noise: Noise,
    last_pitch: Option<f32>,
    volume: LinearRamp,
    cutoff: LinearRamp,
    /// Smoothed osc 1, osc 2 and noise levels and resonance (no zipper noise on knob moves).
    levels: [LinearRamp; 3],
    resonance: LinearRamp,
    drive: LinearRamp,
    shapers: [Shaper; 2],
    /// Low-passed side signal (removed to keep the bass mono).
    side_low: f32,
    controllers: Controllers,
    dirty: bool,
    /// Samples rendered so far (filter updates happen on fixed song-relative samples, so
    /// output does not depend on block sizes).
    counter: u64,
}

impl Default for Polysynth {
    fn default() -> Self {
        Self::new()
    }
}

impl Polysynth {
    /// Synth with default settings.
    pub fn new() -> Self {
        Self {
            values: PARAMS.map(|p| p.default),
            voices: Vec::new(),
            sample_rate: 48_000.0,
            clock: 0,
            lfo_phase: 0.0,
            noise: Noise::default(),
            last_pitch: None,
            volume: LinearRamp::new(0.7, 1),
            cutoff: LinearRamp::new(4_000.0, 1),
            levels: [
                LinearRamp::new(0.8, 1),
                LinearRamp::new(0.0, 1),
                LinearRamp::new(0.0, 1),
            ],
            resonance: LinearRamp::new(0.2, 1),
            drive: LinearRamp::new(0.0, 1),
            shapers: Default::default(),
            side_low: 0.0,
            controllers: Controllers::default(),
            dirty: true,
            counter: 0,
        }
    }

    fn v(&self, id: u64) -> f32 {
        self.values.get(id as usize).copied().unwrap_or(0.0)
    }

    fn update_envelopes(&mut self) {
        let sr = self.sample_rate;
        let (a, d, s, r) = (
            self.v(id::AMP_A),
            self.v(id::AMP_D),
            self.v(id::AMP_S),
            self.v(id::AMP_R),
        );
        let (fa, fd, fs, fr) = (
            self.v(id::FILT_A),
            self.v(id::FILT_D),
            self.v(id::FILT_S),
            self.v(id::FILT_R),
        );
        for v in &mut self.voices {
            v.amp.set(a, d, s, r, sr);
            v.fenv.set(fa, fd, fs, fr, sr);
        }
        self.dirty = false;
    }

    fn note_on(&mut self, key: u8, vel: f32) {
        self.clock += 1;
        let idx = self
            .voices
            .iter()
            .position(|v| !v.amp.is_active())
            .or_else(|| {
                self.voices
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| v.key.is_none())
                    .min_by_key(|(_, v)| v.age)
                    .map(|(i, _)| i)
            })
            .or_else(|| {
                self.voices
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, v)| v.age)
                    .map(|(i, _)| i)
            });
        let sens = self.v(id::VELOCITY);
        let glide = self.v(id::GLIDE) > 0.0;
        let from = self.last_pitch;
        let clock = self.clock;
        let Some(v) = idx.and_then(|i| self.voices.get_mut(i)) else {
            return;
        };
        let p = f32::from(key);
        v.key = Some(key);
        v.target = p;
        v.pitch = match (glide, from) {
            (true, Some(f)) => f,
            _ => p,
        };
        v.vel_gain = 1.0 - sens + sens * vel;
        v.age = clock;
        v.pedal_held = false;
        if self.values.get(id::DRIFT as usize).copied().unwrap_or(0.0) > 0.0 {
            for k in 0..MAX_UNISON {
                let r = v.random();
                if let Some(s) = v.scatter.get_mut(k) {
                    *s = r;
                }
            }
            // A silent voice starts at random phases like a free-running analog
            // oscillator (jumping a sounding one would click).
            if v.amp.level() < 1e-4 {
                for k in 0..MAX_UNISON {
                    let (a, b) = (v.random(), v.random());
                    if let (Some(o1), Some(o2)) = (v.osc1.get_mut(k), v.osc2.get_mut(k)) {
                        *o1 = BlepOsc::with_phase(a);
                        *o2 = BlepOsc::with_phase(b);
                    }
                }
            }
        }
        v.amp.gate_on();
        v.fenv.gate_on();
        self.last_pitch = Some(p);
        self.retune(Some(key));
    }

    /// Sets every held voice's just-intonation correction from the lowest held note. The
    /// voice just started on `fresh` takes it at once; the others slide.
    fn retune(&mut self, fresh: Option<u8>) {
        let on = self.v(id::JUST) > 0.5;
        let root = self.voices.iter().filter_map(|v| v.key).min();
        for v in &mut self.voices {
            let Some(key) = v.key else { continue };
            v.just = match (on, root) {
                (true, Some(r)) => just_offset(i32::from(key) - i32::from(r)),
                _ => 0.0,
            };
            if Some(key) == fresh {
                v.just_now = v.just;
            }
        }
    }

    fn note_off(&mut self, key: u8) {
        let sustain = self.controllers.sustain;
        for v in self
            .voices
            .iter_mut()
            .filter(|v| v.key == Some(key) && !v.pedal_held)
        {
            if sustain {
                v.pedal_held = true;
            } else {
                v.key = None;
                v.amp.gate_off();
                v.fenv.gate_off();
            }
        }
        self.retune(None);
    }

    /// Releases every voice the sustain pedal is holding (all voices with `all`).
    fn release_held(&mut self, all: bool) {
        for v in self
            .voices
            .iter_mut()
            .filter(|v| v.key.is_some() && (all || v.pedal_held))
        {
            v.key = None;
            v.pedal_held = false;
            v.amp.gate_off();
            v.fenv.gate_off();
        }
        self.retune(None);
    }

    fn controller(&mut self, number: u8, val: f32) {
        if self.controllers.set(number, val) == Change::PedalUp {
            self.release_held(false);
        }
    }

    fn render(&mut self, io: &mut AudioIo, from: usize, to: usize) {
        if self.dirty {
            self.update_envelopes();
        }
        let sr = self.sample_rate;
        let w1 = Wave::from_index(self.v(id::OSC1_WAVE));
        let w2 = Wave::from_index(self.v(id::OSC2_WAVE));
        let osc2_semi = self.v(id::OSC2_SEMI).round();
        let osc2_just = if self.v(id::JUST) > 0.5 {
            just_offset(osc2_semi as i32)
        } else {
            0.0
        };
        let osc2_offset = osc2_semi + osc2_just + self.v(id::OSC2_DETUNE) / 100.0;
        let retune_k = 1.0 - (-1.0 / (RETUNE_TIME * sr)).exp();
        let mode = FilterMode::from_index(self.v(id::FILTER_MODE));
        let env_amt = self.v(id::FILTER_ENV) * 6.0;
        let lfo_inc = self.v(id::LFO_RATE) / sr;
        // The mod wheel adds up to half a semitone of vibrato.
        let lfo_pitch = self.v(id::LFO_PITCH) + self.controllers.mod_wheel * MOD_WHEEL_VIBRATO;
        let lfo_cut = self.v(id::LFO_CUTOFF) * 4.0;
        let unison = (self.v(id::UNISON).round() as usize).clamp(1, MAX_UNISON);
        let copies = unison_copies(unison, self.v(id::UNISON_DETUNE), self.v(id::UNISON_SPREAD));
        let stereo = unison > 1 && self.v(id::UNISON_SPREAD) > 0.0;
        let glide = self.v(id::GLIDE);
        let glide_coef = if glide > 0.0 {
            (-4.6 / (glide * sr)).exp()
        } else {
            0.0
        };
        self.volume
            .set_target(self.v(id::VOLUME) * self.controllers.gain());
        self.cutoff.set_target(self.v(id::CUTOFF));
        for (ramp, p) in self
            .levels
            .iter_mut()
            .zip([id::OSC1_LEVEL, id::OSC2_LEVEL, id::NOISE])
        {
            ramp.set_target(self.values.get(p as usize).copied().unwrap_or(0.0));
        }
        self.resonance.set_target(self.v(id::RESONANCE));
        self.drive.set_target(self.v(id::DRIVE));
        let curve = Curve::from_index(self.v(id::DRIVE_TYPE));
        let drift_amt = self.v(id::DRIFT);
        let drift_k = 1.0 / (DRIFT_TIME * sr);
        let mono_k = 1.0 - (-std::f32::consts::TAU * MONO_BASS_HZ / sr).exp();

        for i in from..to {
            let lfo = (self.lfo_phase * std::f32::consts::TAU).sin();
            self.lfo_phase = (self.lfo_phase + lfo_inc).fract();
            let base_cut = self.cutoff.next_value();
            let vol = self.volume.next_value() * HEADROOM;
            let [l1, l2, nl] = self.levels.each_mut().map(LinearRamp::next_value);
            let reso = self.resonance.next_value();
            let update = self.counter.is_multiple_of(FILTER_UPDATE as u64);
            let new_drift = self.counter.is_multiple_of(DRIFT_STEP);
            self.counter += 1;
            let (mut out_l, mut out_r) = (0.0, 0.0);
            for v in &mut self.voices {
                if !v.amp.is_active() {
                    continue;
                }
                v.pitch = v.target + (v.pitch - v.target) * glide_coef;
                if new_drift {
                    v.drift_to = v.random();
                }
                v.drift += (v.drift_to - v.drift) * drift_k;
                v.just_now += (v.just - v.just_now) * retune_k;
                let p = v.pitch + v.just_now + lfo * lfo_pitch + v.drift * DRIFT_WANDER * drift_amt;
                let (mut sl, mut sr_) = (0.0, 0.0);
                for (((o1, o2), c), sc) in v
                    .osc1
                    .iter_mut()
                    .zip(v.osc2.iter_mut())
                    .zip(copies.iter())
                    .zip(v.scatter.iter())
                    .take(unison)
                {
                    let off = c.offset + sc * DRIFT_SCATTER * drift_amt;
                    let hz = 440.0 * ((p + off - 69.0) / 12.0).exp2();
                    let mut s = o1.next_sample(w1, hz / sr) * l1;
                    if l2 > 0.0 {
                        let hz2 = 440.0 * ((p + off + osc2_offset - 69.0) / 12.0).exp2();
                        s += o2.next_sample(w2, hz2 / sr) * l2;
                    }
                    sl += s * c.left;
                    if stereo {
                        sr_ += s * c.right;
                    }
                }
                if nl > 0.0 {
                    let n = self.noise.next_sample() * nl;
                    sl += n;
                    sr_ += n;
                }
                let fe = v.fenv.next_value();
                if update {
                    let cut = base_cut * (fe * env_amt + lfo * lfo_cut).exp2();
                    v.filter.set(cut, reso, sr);
                    if stereo {
                        v.filter_r.set(cut, reso, sr);
                    }
                }
                let a = v.amp.next_value();
                let yl = v.filter.process(sl, mode) * a * v.vel_gain;
                if stereo {
                    out_l += yl;
                    out_r += v.filter_r.process(sr_, mode) * a * v.vel_gain;
                } else {
                    // Keep the right filter in step so turning the width up mid-note
                    // does not click.
                    v.filter_r = v.filter.clone();
                    out_l += yl;
                    out_r += yl;
                }
            }
            // Drive: anti-aliased shaping, crossfaded in over the first 5 % of the knob so
            // that 0 is exactly clean; the make-up gain keeps the level about the same.
            let drive = self.drive.next_value();
            let (mut out_l, mut out_r) = (out_l * HEADROOM, out_r * HEADROOM);
            if drive > 0.0 {
                let g = 10.0_f32.powf(drive * MAX_DRIVE_DB / 20.0);
                let makeup = 0.3 + 0.7 / g;
                let wet = (drive * 20.0).min(1.0);
                let [sh_l, sh_r] = &mut self.shapers;
                out_l += (sh_l.process(out_l * g, curve) * makeup - out_l) * wet;
                out_r += (sh_r.process(out_r * g, curve) * makeup - out_r) * wet;
            }
            // Mono bass: take the low part out of the side signal.
            let mid = 0.5 * (out_l + out_r);
            let side = 0.5 * (out_l - out_r);
            self.side_low += (side - self.side_low) * mono_k;
            let side = side - self.side_low;
            let vol = vol / HEADROOM;
            for (ch, y) in [(mid + side) * vol, (mid - side) * vol]
                .into_iter()
                .enumerate()
            {
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = y;
                }
            }
        }
    }
}

impl AudioNode for Polysynth {
    fn num_inputs(&self) -> usize {
        0
    }

    fn num_outputs(&self) -> usize {
        2
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        self.voices = (0..VOICES as u32).map(Voice::seeded).collect();
        self.volume = LinearRamp::with_time(self.v(id::VOLUME), 20.0, sample_rate);
        self.cutoff = LinearRamp::with_time(self.v(id::CUTOFF), 20.0, sample_rate);
        for (ramp, p) in self
            .levels
            .iter_mut()
            .zip([id::OSC1_LEVEL, id::OSC2_LEVEL, id::NOISE])
        {
            *ramp = LinearRamp::with_time(
                self.values.get(p as usize).copied().unwrap_or(0.0),
                20.0,
                sample_rate,
            );
        }
        self.resonance = LinearRamp::with_time(self.v(id::RESONANCE), 20.0, sample_rate);
        self.drive = LinearRamp::with_time(self.v(id::DRIVE), 20.0, sample_rate);
        self.dirty = true;
    }

    fn process(&mut self, _ctx: &ProcessContext, io: &mut AudioIo, events: &[Event]) {
        let n = io.frames();
        let mut pos = 0;
        for e in events {
            let at = (e.offset as usize).min(n);
            self.render(io, pos, at);
            pos = at;
            match e.kind {
                EventKind::NoteOn { key, vel, .. } => self.note_on(key, vel),
                EventKind::NoteOff { key, .. } => self.note_off(key),
                EventKind::AllNotesOff => {
                    self.controllers.sustain = false;
                    self.release_held(true);
                }
                EventKind::Param { id, val } => self.set_param(id, val),
                EventKind::Cc { cc, val, .. } => self.controller(cc, val),
            }
        }
        self.render(io, pos, n);
    }

    fn reset(&mut self) {
        for (i, v) in self.voices.iter_mut().enumerate() {
            *v = Voice::seeded(i as u32);
        }
        for s in &mut self.shapers {
            s.reset();
        }
        self.side_low = 0.0;
        self.dirty = true;
    }

    fn params(&self) -> &[ParamInfo] {
        &PARAMS
    }

    fn set_param(&mut self, id: ParamId, value: f32) {
        if let (Some(slot), Some(info)) = (
            self.values.get_mut(id.0 as usize),
            PARAMS.get(id.0 as usize),
        ) {
            *slot = value.clamp(info.min.min(info.max), info.max.max(info.min));
            self.dirty = true;
            if id.0 == id::JUST {
                self.retune(None);
            }
        }
    }
}

/// Factory presets.
pub const PRESETS: &[(&str, &[(u64, f32)])] = &[
    ("Init", &[]),
    (
        "Warm pad",
        &[
            (id::OSC2_LEVEL, 0.7),
            (id::OSC2_DETUNE, 12.0),
            (id::CUTOFF, 1_800.0),
            (id::FILTER_ENV, 0.2),
            (id::AMP_A, 0.8),
            (id::AMP_R, 1.5),
            (id::LFO_RATE, 0.3),
            (id::LFO_CUTOFF, 0.3),
        ],
    ),
    (
        "Pluck",
        &[
            (id::OSC1_WAVE, 1.0),
            (id::CUTOFF, 600.0),
            (id::RESONANCE, 0.4),
            (id::FILTER_ENV, 0.7),
            (id::FILT_D, 0.2),
            (id::FILT_S, 0.0),
            (id::AMP_D, 0.4),
            (id::AMP_S, 0.0),
            (id::AMP_R, 0.2),
        ],
    ),
    (
        "Supersaw",
        &[
            (id::UNISON, 7.0),
            (id::UNISON_DETUNE, 25.0),
            (id::UNISON_SPREAD, 0.8),
            (id::CUTOFF, 6_000.0),
            (id::FILTER_ENV, 0.1),
            (id::AMP_A, 0.01),
            (id::AMP_R, 0.6),
        ],
    ),
    (
        "Bass",
        &[
            (id::OSC2_WAVE, 1.0),
            (id::OSC2_LEVEL, 0.6),
            (id::OSC2_SEMI, -12.0),
            (id::OSC2_DETUNE, 0.0),
            (id::CUTOFF, 400.0),
            (id::RESONANCE, 0.3),
            (id::FILTER_ENV, 0.5),
            (id::AMP_S, 0.9),
            (id::AMP_R, 0.1),
            (id::GLIDE, 0.05),
        ],
    ),
    (
        // Matched to the owner's reference ("raw" distorted stabs, made with just-intonation
        // chords of distorted saws): saw + saw an octave up, driven hard through the
        // asymmetric curve with just intonation on, so the difference tones of a played
        // chord land on the overtone series. Play root + fifth (+ octave): e.g. C#2 G#2 C#3.
        "Raw stab",
        &[
            (id::OSC1_LEVEL, 0.9),
            (id::OSC2_WAVE, 0.0),
            (id::OSC2_LEVEL, 0.5),
            (id::OSC2_SEMI, 12.0),
            (id::OSC2_DETUNE, 0.0),
            (id::CUTOFF, 2_500.0),
            (id::RESONANCE, 0.7),
            (id::FILTER_ENV, 0.35),
            (id::FILT_D, 0.15),
            (id::FILT_S, 0.3),
            (id::AMP_A, 0.002),
            (id::AMP_D, 0.18),
            (id::AMP_S, 0.35),
            (id::AMP_R, 0.06),
            (id::DRIVE, 0.85),
            (id::DRIVE_TYPE, 2.0),
            (id::DRIFT, 0.2),
            (id::JUST, 1.0),
        ],
    ),
    (
        "Raw reese",
        &[
            (id::OSC2_LEVEL, 0.8),
            (id::OSC2_DETUNE, 18.0),
            (id::UNISON, 3.0),
            (id::UNISON_DETUNE, 12.0),
            (id::UNISON_SPREAD, 0.3),
            (id::CUTOFF, 900.0),
            (id::RESONANCE, 0.3),
            (id::FILTER_ENV, 0.2),
            (id::AMP_A, 0.005),
            (id::AMP_S, 1.0),
            (id::AMP_R, 0.1),
            (id::GLIDE, 0.05),
            (id::DRIVE, 0.7),
            (id::DRIVE_TYPE, 0.0),
        ],
    ),
    (
        "Dirty sub",
        &[
            (id::OSC1_WAVE, 3.0),
            (id::OSC2_WAVE, 2.0),
            (id::OSC2_LEVEL, 0.3),
            (id::OSC2_SEMI, 12.0),
            (id::OSC2_DETUNE, 0.0),
            (id::CUTOFF, 3_000.0),
            (id::FILTER_ENV, 0.0),
            (id::AMP_S, 1.0),
            (id::AMP_R, 0.15),
            (id::GLIDE, 0.04),
            (id::DRIVE, 0.6),
            (id::DRIVE_TYPE, 0.0),
            (id::DRIFT, 0.0),
        ],
    ),
    (
        // The 2022/23 phonk cowbell: the 808 cowbell (two square waves a little under a
        // fifth apart, 540 : 800 Hz, through a band-pass), played as a melody.
        "Phonk cowbell",
        &[
            (id::OSC1_WAVE, 1.0),
            (id::OSC1_LEVEL, 0.8),
            (id::OSC2_WAVE, 1.0),
            (id::OSC2_LEVEL, 0.8),
            (id::OSC2_SEMI, 7.0),
            (id::OSC2_DETUNE, -20.0),
            (id::FILTER_MODE, 2.0),
            (id::CUTOFF, 2_200.0),
            (id::RESONANCE, 0.3),
            (id::FILTER_ENV, 0.0),
            (id::AMP_A, 0.001),
            (id::AMP_D, 0.35),
            (id::AMP_S, 0.0),
            (id::AMP_R, 0.15),
            (id::VELOCITY, 0.5),
            (id::DRIVE, 0.4),
            (id::DRIVE_TYPE, 1.0),
            (id::DRIFT, 0.0),
        ],
    ),
    (
        "Phonk 808",
        &[
            (id::OSC1_WAVE, 3.0),
            (id::OSC1_LEVEL, 1.0),
            (id::OSC2_WAVE, 2.0),
            (id::OSC2_LEVEL, 0.25),
            (id::OSC2_SEMI, 12.0),
            (id::OSC2_DETUNE, 0.0),
            (id::CUTOFF, 2_500.0),
            (id::FILTER_ENV, 0.0),
            (id::AMP_A, 0.002),
            (id::AMP_D, 1.2),
            (id::AMP_S, 0.4),
            (id::AMP_R, 0.25),
            (id::GLIDE, 0.06),
            (id::DRIVE, 0.55),
            (id::DRIVE_TYPE, 0.0),
            (id::DRIFT, 0.0),
        ],
    ),
    (
        "Tekk lead",
        &[
            (id::OSC2_WAVE, 0.0),
            (id::OSC2_LEVEL, 0.5),
            (id::OSC2_SEMI, 12.0),
            (id::OSC2_DETUNE, 5.0),
            (id::UNISON, 3.0),
            (id::UNISON_DETUNE, 15.0),
            (id::UNISON_SPREAD, 0.5),
            (id::CUTOFF, 5_000.0),
            (id::RESONANCE, 0.2),
            (id::FILTER_ENV, 0.2),
            (id::AMP_A, 0.002),
            (id::AMP_D, 0.3),
            (id::AMP_S, 0.6),
            (id::AMP_R, 0.15),
            (id::DRIVE, 0.3),
        ],
    ),
    (
        "Hyper supersaw",
        &[
            (id::UNISON, 7.0),
            (id::UNISON_DETUNE, 35.0),
            (id::UNISON_SPREAD, 1.0),
            (id::CUTOFF, 9_000.0),
            (id::FILTER_ENV, 0.1),
            (id::AMP_A, 0.005),
            (id::AMP_R, 0.3),
            (id::DRIVE, 0.35),
        ],
    ),
    // EDM sounds, matched to the owner's reference songs (drops: kick carries the sub,
    // a sustained bass pumps against it, leads bright around 1.3–1.8 kHz and wide).
    (
        "EDM bass",
        &[
            (id::OSC2_WAVE, 1.0),
            (id::OSC2_LEVEL, 0.6),
            (id::OSC2_SEMI, -12.0),
            (id::OSC2_DETUNE, 0.0),
            (id::CUTOFF, 700.0),
            (id::RESONANCE, 0.2),
            (id::FILTER_ENV, 0.15),
            (id::AMP_A, 0.003),
            (id::AMP_S, 1.0),
            (id::AMP_R, 0.08),
            (id::DRIVE, 0.3),
            (id::DRIFT, 0.1),
        ],
    ),
    (
        "Big room lead",
        &[
            (id::OSC2_WAVE, 0.0),
            (id::OSC2_LEVEL, 0.6),
            (id::OSC2_SEMI, 12.0),
            (id::OSC2_DETUNE, 6.0),
            (id::UNISON, 7.0),
            (id::UNISON_DETUNE, 30.0),
            (id::UNISON_SPREAD, 1.0),
            (id::CUTOFF, 8_000.0),
            (id::RESONANCE, 0.15),
            (id::FILTER_ENV, 0.1),
            (id::AMP_A, 0.003),
            (id::AMP_D, 0.4),
            (id::AMP_S, 0.8),
            (id::AMP_R, 0.25),
            (id::DRIVE, 0.3),
        ],
    ),
    (
        "Melodic pluck",
        &[
            (id::OSC2_WAVE, 1.0),
            (id::OSC2_LEVEL, 0.5),
            (id::OSC2_DETUNE, 9.0),
            (id::UNISON, 3.0),
            (id::UNISON_DETUNE, 14.0),
            (id::UNISON_SPREAD, 0.7),
            (id::CUTOFF, 900.0),
            (id::RESONANCE, 0.25),
            (id::FILTER_ENV, 0.6),
            (id::FILT_D, 0.25),
            (id::FILT_S, 0.1),
            (id::AMP_A, 0.002),
            (id::AMP_D, 0.35),
            (id::AMP_S, 0.15),
            (id::AMP_R, 0.25),
        ],
    ),
    (
        "Hard lead",
        &[
            (id::OSC2_WAVE, 1.0),
            (id::OSC2_LEVEL, 0.5),
            (id::OSC2_DETUNE, 12.0),
            (id::UNISON, 3.0),
            (id::UNISON_DETUNE, 18.0),
            (id::UNISON_SPREAD, 0.6),
            (id::CUTOFF, 3_500.0),
            (id::RESONANCE, 0.35),
            (id::FILTER_ENV, 0.2),
            (id::AMP_A, 0.002),
            (id::AMP_S, 0.9),
            (id::AMP_R, 0.12),
            (id::DRIVE, 0.7),
            (id::DRIVE_TYPE, 1.0),
        ],
    ),
    (
        // White-noise sweep for build-ups (automate the cutoff and volume upwards).
        "Riser",
        &[
            (id::OSC1_LEVEL, 0.0),
            (id::NOISE, 1.0),
            (id::CUTOFF, 400.0),
            (id::RESONANCE, 0.45),
            (id::FILTER_ENV, 0.0),
            (id::AMP_A, 0.5),
            (id::AMP_S, 1.0),
            (id::AMP_R, 0.6),
            (id::DRIFT, 0.0),
        ],
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use musiton_core::{TempoMap, TimeSig};

    fn render(s: &mut Polysynth, events: &[Event], frames: usize) -> Vec<f32> {
        render_stereo(s, events, frames).swap_remove(0)
    }

    fn render_stereo(s: &mut Polysynth, events: &[Event], frames: usize) -> Vec<Vec<f32>> {
        let tempo = TempoMap::default();
        let mut outs = vec![vec![0.0; frames]; 2];
        let ctx = ProcessContext {
            sample_rate: 48_000.0,
            frames,
            playing: true,
            sample_pos: 0,
            tempo: &tempo,
            time_sig: TimeSig::default(),
            metronome: false,
            inputs: &[],
            recording: false,
            punch: None,
            live_midi: &[],
        };
        s.process(&ctx, &mut AudioIo::new(&[], &mut outs, frames), events);
        outs
    }

    fn synth(settings: &[(u64, f32)]) -> Polysynth {
        let mut s = Polysynth::new();
        for &(i, v) in settings {
            s.set_param(ParamId(i), v);
        }
        s.prepare(48_000.0, 48_000);
        s
    }

    fn on(offset: u32, key: u8) -> Event {
        Event {
            offset,
            kind: EventKind::NoteOn {
                ch: 0,
                key,
                vel: 1.0,
            },
        }
    }

    fn magnitude(x: &[f32], hz: f64) -> f64 {
        let (mut re, mut im) = (0.0, 0.0);
        for (n, &s) in x.iter().enumerate() {
            let w = std::f64::consts::TAU * hz * n as f64 / 48_000.0;
            let win = 0.5 - 0.5 * (std::f64::consts::TAU * n as f64 / x.len() as f64).cos();
            re += f64::from(s) * w.cos() * win;
            im -= f64::from(s) * w.sin() * win;
        }
        (re * re + im * im).sqrt()
    }

    fn ev(offset: u32, kind: EventKind) -> Event {
        Event { offset, kind }
    }

    fn cc(offset: u32, cc: u8, val: f32) -> Event {
        ev(offset, EventKind::Cc { ch: 0, cc, val })
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn sustain_pedal_holds_notes_until_it_is_released() {
        let settings = [(id::AMP_S, 1.0), (id::AMP_R, 0.01)];
        let off = |offset| ev(offset, EventKind::NoteOff { ch: 0, key: 60 });
        // Without the pedal the note is gone shortly after the key is released.
        let mut s = synth(&settings);
        let out = render(&mut s, &[on(0, 60), off(4_800)], 24_000);
        assert!(rms(&out[12_000..14_400]) < 1e-4);
        // With the pedal down it keeps sounding until the pedal comes up.
        let mut s = synth(&settings);
        let events = [
            on(0, 60),
            cc(2_400, crate::controllers::cc::SUSTAIN, 1.0),
            off(4_800),
            cc(16_800, crate::controllers::cc::SUSTAIN, 0.0),
        ];
        let out = render(&mut s, &events, 24_000);
        assert!(rms(&out[12_000..14_400]) > 0.01, "note not held");
        assert!(rms(&out[21_600..24_000]) < 1e-4, "note not released");
    }

    #[test]
    fn volume_controller_scales_the_level_and_mod_wheel_adds_vibrato() {
        let settings = [
            (id::OSC1_WAVE, 3.0),
            (id::CUTOFF, 20_000.0),
            (id::FILTER_ENV, 0.0),
            (id::AMP_S, 1.0),
            (id::LFO_RATE, 6.0),
        ];
        let mut s = synth(&settings);
        let full = rms(&render(&mut s, &[on(0, 69)], 24_000)[12_000..]);
        let mut s = synth(&settings);
        let events = [cc(0, crate::controllers::cc::VOLUME, 0.5), on(0, 69)];
        let half = rms(&render(&mut s, &events, 24_000)[12_000..]);
        // Volume is squared: 0.5 → a quarter of the level.
        assert!((half / full - 0.25).abs() < 0.01, "{}", half / full);

        // Vibrato: the period between zero crossings starts to vary.
        let spread = |out: &[f32]| {
            let zc: Vec<usize> = (1..out.len())
                .filter(|&i| out[i - 1] < 0.0 && out[i] >= 0.0)
                .collect();
            let periods: Vec<usize> = zc.windows(2).map(|w| w[1] - w[0]).collect();
            periods.iter().max().unwrap() - periods.iter().min().unwrap()
        };
        let mut s = synth(&settings);
        let steady = spread(&render(&mut s, &[on(0, 69)], 48_000)[4_800..]);
        let mut s = synth(&settings);
        let events = [cc(0, crate::controllers::cc::MOD_WHEEL, 1.0), on(0, 69)];
        let wobble = spread(&render(&mut s, &events, 48_000)[4_800..]);
        assert!(steady <= 1, "steady {steady}");
        assert!(wobble >= 3, "wobble {wobble}");
    }

    #[test]
    fn plays_the_right_pitch() {
        let mut s = synth(&[
            (id::OSC1_WAVE, 3.0),
            (id::CUTOFF, 20_000.0),
            (id::FILTER_ENV, 0.0),
        ]);
        let out = render(&mut s, &[on(0, 69)], 48_000);
        let crossings = out[4_800..43_200]
            .windows(2)
            .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
            .count();
        assert!((351..=353).contains(&crossings), "{crossings}");
    }

    #[test]
    fn high_notes_do_not_alias() {
        // Key 120 ≈ 8372 Hz. Aliases of a naive saw would appear at ≈2.2 and 6.1 kHz.
        let mut s = synth(&[
            (id::CUTOFF, 20_000.0),
            (id::FILTER_ENV, 0.0),
            (id::RESONANCE, 0.0),
        ]);
        let out = render(&mut s, &[on(0, 120)], 24_000);
        let x = &out[4_800..14_400];
        let fund = magnitude(x, 8_372.0);
        for alias in [2_163.0, 6_118.0] {
            let db = 20.0 * (magnitude(x, alias) / fund).log10();
            assert!(db < -30.0, "alias at {alias} Hz: {db:.1} dB");
        }
    }

    #[test]
    fn retriggers_steals_and_releases_without_clicks() {
        // Sine oscillator so any jump in the output is a click.
        let mut s = synth(&[
            (id::OSC1_WAVE, 3.0),
            (id::CUTOFF, 20_000.0),
            (id::FILTER_ENV, 0.0),
            (id::AMP_R, 0.05),
        ]);
        let mut events: Vec<Event> = (0..24).map(|k| on(k * 500, 48 + (k % 5) as u8)).collect();
        events.extend((0..24).map(|k| Event {
            offset: 20_000 + k * 7,
            kind: EventKind::NoteOff {
                ch: 0,
                key: 48 + (k % 5) as u8,
            },
        }));
        let out = render(&mut s, &events, 48_000);
        let worst = out
            .windows(2)
            .fold(0.0_f32, |m, w| m.max((w[1] - w[0]).abs()));
        assert!(worst < 0.02, "click of {worst}");
        assert!(out[47_000..].iter().all(|x| x.abs() < 1e-3));
    }

    #[test]
    fn unison_detunes_and_spreads_the_copies() {
        let base = [
            (id::CUTOFF, 20_000.0),
            (id::FILTER_ENV, 0.0),
            (id::AMP_S, 1.0),
        ];
        let stereo =
            |settings: &[(u64, f32)]| render_stereo(&mut synth(settings), &[on(0, 57)], 48_000);
        // One copy: both sides are the same.
        let one = stereo(&base);
        assert_eq!(one[0], one[1]);
        // Seven copies 50 cents apart and fully spread: the sides differ, the level
        // stays close to a single copy and energy appears next to the fundamental.
        let mut wide = base.to_vec();
        wide.extend([
            (id::UNISON, 7.0),
            (id::UNISON_DETUNE, 50.0),
            (id::UNISON_SPREAD, 1.0),
        ]);
        let seven = stereo(&wide);
        let diff = rms(&seven[0]
            .iter()
            .zip(&seven[1])
            .map(|(l, r)| l - r)
            .collect::<Vec<_>>());
        assert!(diff > 0.01, "sides too alike: {diff}");
        let ratio = rms(&seven[0][4_800..]) / rms(&one[0][4_800..]);
        assert!((0.5..1.5).contains(&ratio), "level ratio {ratio}");
        // 220 Hz ± 50 cents ≈ 213.7 … 226.5 Hz.
        let side = |x: &[f32]| magnitude(&x[4_800..43_200], 213.7);
        assert!(side(&seven[0]) > side(&one[0]) * 10.0);
    }

    #[test]
    fn just_offsets_are_pure_intervals() {
        assert!(just_offset(0).abs() < 1e-6);
        assert!((just_offset(7) - 0.019_55).abs() < 1e-4, "fifth");
        assert!((just_offset(4) + 0.136_86).abs() < 1e-4, "major third");
        assert!((just_offset(19) - just_offset(7)).abs() < 1e-6, "octaves");
        assert!(
            (just_offset(-5) - just_offset(7)).abs() < 1e-6,
            "below the root"
        );
    }

    #[test]
    fn just_intonation_retunes_the_chord() {
        // C5 + E5: equal-tempered E5 is 659.26 Hz, the just major third 654.06 Hz.
        let play = |just: f32| {
            render(
                &mut synth(&[
                    (id::OSC1_WAVE, 3.0),
                    (id::CUTOFF, 20_000.0),
                    (id::FILTER_ENV, 0.0),
                    (id::DRIFT, 0.0),
                    (id::JUST, just),
                ]),
                &[on(0, 72), on(0, 76)],
                48_000,
            )
        };
        let (et, ji) = (play(0.0), play(1.0));
        let m = |x: &[f32], hz| magnitude(&x[9_600..48_000], hz);
        assert!(m(&ji, 654.06) > 4.0 * m(&ji, 659.26));
        assert!(m(&et, 659.26) > 4.0 * m(&et, 654.06));
        // The root stays where it is.
        assert!(m(&ji, 523.25) > 4.0 * m(&ji, 528.0));
    }

    #[test]
    fn drive_adds_harmonics() {
        let play = |drive: f32| {
            render(
                &mut synth(&[
                    (id::OSC1_WAVE, 3.0),
                    (id::CUTOFF, 20_000.0),
                    (id::FILTER_ENV, 0.0),
                    (id::DRIVE, drive),
                ]),
                &[on(0, 45)],
                24_000,
            )
        };
        let (clean, driven) = (play(0.0), play(0.8));
        let third =
            |x: &[f32]| magnitude(&x[4_800..24_000], 330.0) / magnitude(&x[4_800..24_000], 110.0);
        assert!(third(&clean) < 0.01, "clean {}", third(&clean));
        assert!(third(&driven) > 0.1, "driven {}", third(&driven));
        let peak = driven.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!(peak < 1.0, "peak {peak}");
    }

    #[test]
    fn low_cutoff_removes_highs() {
        let bright = render(
            &mut synth(&[(id::CUTOFF, 20_000.0), (id::FILTER_ENV, 0.0)]),
            &[on(0, 45)],
            24_000,
        );
        let dark = render(
            &mut synth(&[(id::CUTOFF, 300.0), (id::FILTER_ENV, 0.0)]),
            &[on(0, 45)],
            24_000,
        );
        let h = |x: &[f32]| magnitude(&x[4_800..14_400], 3_520.0);
        assert!(h(&dark) < h(&bright) * 0.01);
    }
}
