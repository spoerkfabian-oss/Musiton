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
pub const MAX_UNISON: usize = 9;

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

/// Overtone tuning: each semitone becomes the nearest harmonic of the root (up to the
/// 27th), so chords are slices of one overtone series (11:8, 13:8, 7:4 …).
const OVERTONE_RATIOS: [f32; 12] = [
    1.0,
    17.0 / 16.0,
    9.0 / 8.0,
    19.0 / 16.0,
    5.0 / 4.0,
    21.0 / 16.0,
    11.0 / 8.0,
    3.0 / 2.0,
    13.0 / 8.0,
    27.0 / 16.0,
    7.0 / 4.0,
    15.0 / 8.0,
];

/// Correction (semitones) that turns the equal-tempered interval `semis` into its just
/// version, e.g. +7 → +0.02 (a pure fifth is 2 cents wider); `overtone` uses the
/// overtone-series table instead.
fn just_offset(semis: i32, overtone: bool) -> f32 {
    let class = semis.rem_euclid(12);
    let table = if overtone {
        &OVERTONE_RATIOS
    } else {
        &JUST_RATIOS
    };
    let ratio = table.get(class as usize).copied().unwrap_or(1.0);
    12.0 * ratio.log2() - class as f32
}

/// Seconds after a note-on during which a retune takes effect at once (the notes of one
/// chord arrive in any order; only later changes slide).
const CHORD_WINDOW: f32 = 0.005;
/// Delay (seconds) of the stereo widener's high band.
const WIDTH_DELAY: f32 = 0.0005;
/// The widener works above this frequency (Hz), so the low end stays centred.
const WIDTH_HZ: f32 = 1_500.0;
/// Ring buffer for the widener's delay (enough for 0.5 ms at 192 kHz).
const WIDTH_BUF: usize = 128;
const ROOTS: &[&str] = &["Lowest note", "Most played", "Fixed key"];
const KEYS: &[&str] = &[
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];
const BEND_FROM: &[&str] = &["Bottom", "Top"];
/// Choosable ratios per interval above the root (index = semitones − 1); the first
/// option, "Table", keeps the ratio of the "Tuning" table.
const INTERVAL_NAMES: [&[&str]; 11] = [
    &["Table", "16/15", "17/16", "15/14", "25/24", "256/243"],
    &["Table", "9/8", "10/9", "8/7"],
    &["Table", "6/5", "19/16", "7/6", "32/27"],
    &["Table", "5/4", "81/64", "9/7", "14/11"],
    &["Table", "4/3", "21/16", "27/20"],
    &["Table", "45/32", "7/5", "11/8", "10/7", "64/45"],
    &["Table", "3/2", "40/27"],
    &["Table", "8/5", "13/8", "25/16", "14/9"],
    &["Table", "5/3", "27/16", "12/7"],
    &["Table", "9/5", "7/4", "16/9"],
    &["Table", "15/8", "243/128", "17/9"],
];
/// The ratios behind [`INTERVAL_NAMES`] (without "Table").
const INTERVAL_RATIOS: [&[f32]; 11] = [
    &[
        16.0 / 15.0,
        17.0 / 16.0,
        15.0 / 14.0,
        25.0 / 24.0,
        256.0 / 243.0,
    ],
    &[9.0 / 8.0, 10.0 / 9.0, 8.0 / 7.0],
    &[6.0 / 5.0, 19.0 / 16.0, 7.0 / 6.0, 32.0 / 27.0],
    &[5.0 / 4.0, 81.0 / 64.0, 9.0 / 7.0, 14.0 / 11.0],
    &[4.0 / 3.0, 21.0 / 16.0, 27.0 / 20.0],
    &[45.0 / 32.0, 7.0 / 5.0, 11.0 / 8.0, 10.0 / 7.0, 64.0 / 45.0],
    &[3.0 / 2.0, 40.0 / 27.0],
    &[8.0 / 5.0, 13.0 / 8.0, 25.0 / 16.0, 14.0 / 9.0],
    &[5.0 / 3.0, 27.0 / 16.0, 12.0 / 7.0],
    &[9.0 / 5.0, 7.0 / 4.0, 16.0 / 9.0],
    &[15.0 / 8.0, 243.0 / 128.0, 17.0 / 9.0],
];
const SLOPES: &[&str] = &["12 dB", "24 dB"];
const GLIDES: &[&str] = &["From last note", "Each voice"];
const TUNINGS: &[&str] = &["Just", "Overtone series"];
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
    pub const JUST_ROOT: u64 = 32;
    pub const JUST_KEYS: u64 = 33;
    pub const TUNING: u64 = 34;
    pub const BEND_RANGE: u64 = 35;
    pub const BEND_NOTE: u64 = 36;
    pub const WIDTH: u64 = 37;
    pub const OSC3_LEVEL: u64 = 38;
    pub const OSC3_SEMI: u64 = 39;
    pub const SUB: u64 = 40;
    pub const FILTER_SLOPE: u64 = 41;
    pub const PITCH_ENV: u64 = 42;
    pub const PITCH_DECAY: u64 = 43;
    pub const GLIDE_MODE: u64 = 44;
    pub const JUST_KEY: u64 = 45;
    pub const BEND_FROM: u64 = 46;
    /// First of eleven interval ratios (minor second … major seventh above the root).
    pub const INTERVAL_M2: u64 = 47;
}

/// All parameters (index = id).
pub const PARAMS: [ParamInfo; 58] = [
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
    ParamInfo::choice(id::JUST_ROOT, "Just root", ROOTS, 0),
    ParamInfo::new(id::JUST_KEYS, "Retune from keys", 1.0, 8.0, 1.0).int(),
    ParamInfo::choice(id::TUNING, "Tuning", TUNINGS, 0),
    ParamInfo::new(id::BEND_RANGE, "Bend range", 0.0, 24.0, 2.0).unit("st"),
    ParamInfo::new(id::BEND_NOTE, "Bend note (0 = all)", 0.0, 8.0, 0.0).int(),
    ParamInfo::new(id::WIDTH, "High width", 0.0, 1.0, 0.0),
    ParamInfo::new(id::OSC3_LEVEL, "Osc 3 level", 0.0, 1.0, 0.0),
    ParamInfo::new(id::OSC3_SEMI, "Osc 3 semitones", -24.0, 24.0, 19.0).int(),
    ParamInfo::new(id::SUB, "Sub (sine −1 oct)", 0.0, 1.0, 0.0),
    ParamInfo::choice(id::FILTER_SLOPE, "Filter slope", SLOPES, 0),
    ParamInfo::new(id::PITCH_ENV, "Pitch env", -48.0, 48.0, 0.0).unit("st"),
    ParamInfo::new(id::PITCH_DECAY, "Pitch decay", 0.001, 2.0, 0.05)
        .log()
        .unit("s"),
    ParamInfo::choice(id::GLIDE_MODE, "Glide mode", GLIDES, 0),
    ParamInfo::choice(id::JUST_KEY, "Fixed root key", KEYS, 0),
    ParamInfo::choice(id::BEND_FROM, "Bend note counted from", BEND_FROM, 0),
    ParamInfo::choice(id::INTERVAL_M2, "Ratio minor 2nd", INTERVAL_NAMES[0], 0),
    ParamInfo::choice(id::INTERVAL_M2 + 1, "Ratio major 2nd", INTERVAL_NAMES[1], 0),
    ParamInfo::choice(id::INTERVAL_M2 + 2, "Ratio minor 3rd", INTERVAL_NAMES[2], 0),
    ParamInfo::choice(id::INTERVAL_M2 + 3, "Ratio major 3rd", INTERVAL_NAMES[3], 0),
    ParamInfo::choice(id::INTERVAL_M2 + 4, "Ratio 4th", INTERVAL_NAMES[4], 0),
    ParamInfo::choice(id::INTERVAL_M2 + 5, "Ratio tritone", INTERVAL_NAMES[5], 0),
    ParamInfo::choice(id::INTERVAL_M2 + 6, "Ratio 5th", INTERVAL_NAMES[6], 0),
    ParamInfo::choice(id::INTERVAL_M2 + 7, "Ratio minor 6th", INTERVAL_NAMES[7], 0),
    ParamInfo::choice(id::INTERVAL_M2 + 8, "Ratio major 6th", INTERVAL_NAMES[8], 0),
    ParamInfo::choice(id::INTERVAL_M2 + 9, "Ratio minor 7th", INTERVAL_NAMES[9], 0),
    ParamInfo::choice(
        id::INTERVAL_M2 + 10,
        "Ratio major 7th",
        INTERVAL_NAMES[10],
        0,
    ),
];

#[derive(Debug, Clone)]
struct Voice {
    key: Option<u8>,
    osc1: [BlepOsc; MAX_UNISON],
    osc2: [BlepOsc; MAX_UNISON],
    /// Third oscillator (osc 1's wave at its own interval).
    osc3: [BlepOsc; MAX_UNISON],
    /// Sub oscillator phase (turns).
    sub: f32,
    /// Pitch envelope level (1 at note-on, decays to 0).
    penv: f32,
    /// Second filter stages for the 24 dB slope.
    filter2: Svf,
    filter2_r: Svf,
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
    /// Samples left in which a retune applies at once (see [`CHORD_WINDOW`]).
    fresh: u32,
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
            osc2: phases.clone(),
            osc3: phases,
            sub: 0.0,
            penv: 0.0,
            filter: Svf::default(),
            filter_r: Svf::default(),
            filter2: Svf::default(),
            filter2_r: Svf::default(),
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
            fresh: 0,
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
    /// Widener: low-pass state (its high band is the input minus this) and the delay.
    wide_low: [f32; 2],
    wide_buf: [f32; WIDTH_BUF],
    wide_pos: usize,
    /// How often each pitch class was played recently (for "Most played" roots).
    played: [f32; 12],
    /// The root of the last chord with enough keys.
    root: Option<u8>,
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
            wide_low: [0.0; 2],
            wide_buf: [0.0; WIDTH_BUF],
            wide_pos: 0,
            played: [0.0; 12],
            root: None,
            controllers: Controllers::default(),
            dirty: true,
            counter: 0,
        }
    }

    fn v(&self, id: u64) -> f32 {
        self.values.get(id as usize).copied().unwrap_or(0.0)
    }

    /// Just correction (semitones) of `semis` above the root: the interval's chosen ratio,
    /// or the "Tuning" table's.
    fn just_for(&self, semis: i32) -> f32 {
        let class = semis.rem_euclid(12) as usize;
        let chosen = class
            .checked_sub(1)
            .and_then(|i| {
                let pick = self.v(id::INTERVAL_M2 + i as u64).round() as usize;
                INTERVAL_RATIOS.get(i)?.get(pick.checked_sub(1)?)
            })
            .copied();
        match chosen {
            Some(ratio) => 12.0 * ratio.log2() - class as f32,
            None => just_offset(semis, self.v(id::TUNING) > 0.5),
        }
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
        let just = self.v(id::JUST) > 0.5;
        let fresh = (CHORD_WINDOW * self.sample_rate) as u32;
        let mut from = self.last_pitch;
        // "Each voice" glide: a new note takes over the released voice nearest to it and
        // slides from there, so a whole chord glides into the next one.
        let mut idx = idx;
        if glide && self.v(id::GLIDE_MODE) > 0.5 {
            let target = f32::from(key);
            let nearest = self
                .voices
                .iter()
                .enumerate()
                .filter(|(_, v)| v.key.is_none() && v.amp.is_active())
                .min_by(|(_, a), (_, b)| {
                    (a.pitch - target)
                        .abs()
                        .total_cmp(&(b.pitch - target).abs())
                })
                .map(|(i, v)| (i, v.pitch));
            if let Some((i, p)) = nearest {
                idx = Some(i);
                from = Some(p);
            }
        }
        let clock = self.clock;
        for c in &mut self.played {
            *c *= 0.85;
        }
        if let Some(c) = self.played.get_mut(usize::from(key % 12)) {
            *c += 1.0;
        }
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
        v.penv = 1.0;
        v.age = clock;
        v.pedal_held = false;
        v.fresh = fresh;
        if just && v.amp.level() < 1e-4 {
            // Just intonation locks the partials of a chord together; fixed start phases
            // make every stab of it sound the same.
            for (o1, o2) in v.osc1.iter_mut().zip(v.osc2.iter_mut()) {
                *o1 = BlepOsc::with_phase(0.0);
                *o2 = BlepOsc::with_phase(0.0);
            }
        } else if self.values.get(id::DRIFT as usize).copied().unwrap_or(0.0) > 0.0 {
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
        if glide && self.v(id::GLIDE_MODE) > 0.5 {
            self.order_glides();
        }
        self.retune(Some(key));
    }

    /// "Each voice" glide: the notes of a chord arrive one by one, so the nearest-voice
    /// choice can cross. Reassign the starting pitches of the voices started within
    /// [`CHORD_WINDOW`] in order: the lowest new note slides from the lowest old one.
    fn order_glides(&mut self) {
        let mut idx = [0_usize; VOICES];
        let mut from = [0.0_f32; VOICES];
        let mut n = 0;
        for (i, v) in self.voices.iter().enumerate() {
            if v.key.is_some() && v.fresh > 0 {
                if let (Some(a), Some(b)) = (idx.get_mut(n), from.get_mut(n)) {
                    *a = i;
                    *b = v.pitch;
                    n += 1;
                }
            }
        }
        let (Some(idx), Some(from)) = (idx.get_mut(..n), from.get_mut(..n)) else {
            return;
        };
        from.sort_unstable_by(f32::total_cmp);
        idx.sort_unstable_by(|&a, &b| {
            let t = |i: usize| self.voices.get(i).map_or(0.0, |v| v.target);
            t(a).total_cmp(&t(b))
        });
        for (&i, &f) in idx.iter().zip(from.iter()) {
            if let Some(v) = self.voices.get_mut(i) {
                v.pitch = f;
            }
        }
    }

    /// Sets every held voice's just-intonation correction from the chord's root: the
    /// lowest held note or the most played one, recalculated only while at least "Retune
    /// from keys" notes are held (fewer keep the last root, so melody notes over a chord
    /// are tuned to it). Voices started within [`CHORD_WINDOW`] take the new tuning at
    /// once; the others slide.
    fn retune(&mut self, fresh: Option<u8>) {
        let on = self.v(id::JUST) > 0.5;
        let mode = self.v(id::JUST_ROOT).round() as u8;
        let held = self.voices.iter().filter(|v| v.key.is_some()).count();
        let lowest = self.voices.iter().filter_map(|v| v.key).min();
        // With no key held the last root stays (a melody after a chord is tuned to it).
        if mode == 2 {
            // Fixed key: no dynamic retuning.
            self.root = Some(self.v(id::JUST_KEY).round().clamp(0.0, 11.0) as u8);
        } else if held > 0
            && (held >= self.v(id::JUST_KEYS).round().max(1.0) as usize || self.root.is_none())
        {
            self.root = if mode == 1 {
                let weight = |k: u8| self.played.get(usize::from(k % 12)).copied().unwrap_or(0.0);
                // Most played pitch class; the lowest such note on a tie.
                self.voices
                    .iter()
                    .filter_map(|v| v.key)
                    .fold(None, |best, k| match best {
                        Some(b) if weight(b) > weight(k) || (weight(b) == weight(k) && b < k) => {
                            Some(b)
                        }
                        _ => Some(k),
                    })
            } else {
                lowest
            };
        }
        let root = self.root;
        for i in 0..self.voices.len() {
            let Some(key) = self.voices.get(i).and_then(|v| v.key) else {
                continue;
            };
            let just = match (on, root) {
                (true, Some(r)) => self.just_for(i32::from(key) - i32::from(r)),
                _ => 0.0,
            };
            let Some(v) = self.voices.get_mut(i) else {
                continue;
            };
            v.just = just;
            if Some(key) == fresh || v.fresh > 0 {
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
            self.just_for(osc2_semi as i32)
        } else {
            0.0
        };
        let osc2_offset = osc2_semi + osc2_just + self.v(id::OSC2_DETUNE) / 100.0;
        let osc3_semi = self.v(id::OSC3_SEMI).round();
        let osc3_offset = osc3_semi
            + if self.v(id::JUST) > 0.5 {
                self.just_for(osc3_semi as i32)
            } else {
                0.0
            };
        let l3 = self.v(id::OSC3_LEVEL);
        let sub_level = self.v(id::SUB);
        let steep = self.v(id::FILTER_SLOPE) > 0.5;
        let pitch_env = self.v(id::PITCH_ENV);
        let penv_coef = (-1.0 / (self.v(id::PITCH_DECAY).max(0.001) * sr)).exp();
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
        // Pitch bend: the whole chord, or only its n-th note from the bottom or top.
        let bend = self.controllers.pitch_bend * self.v(id::BEND_RANGE);
        let bend_note = self.v(id::BEND_NOTE).round() as usize;
        let from_top = self.v(id::BEND_FROM) > 0.5;
        let bend_key = if bend_note == 0 {
            None
        } else {
            let keys = || self.voices.iter().filter_map(|v| v.key);
            let below = |k: u8| {
                keys()
                    .filter(|&o| if from_top { o > k } else { o < k })
                    .count()
            };
            keys().find(|&k| below(k) + 1 == bend_note)
        };
        let width = self.v(id::WIDTH);
        let wide_k = 1.0 - (-std::f32::consts::TAU * WIDTH_HZ / sr).exp();
        let wide_delay = ((WIDTH_DELAY * sr) as usize).clamp(1, WIDTH_BUF - 1);

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
                v.fresh = v.fresh.saturating_sub(1);
                let bent = match bend_key {
                    None => bend,
                    Some(k) if v.key == Some(k) => bend,
                    Some(_) => 0.0,
                };
                let p = v.pitch
                    + v.just_now
                    + bent
                    + v.penv * pitch_env
                    + lfo * lfo_pitch
                    + v.drift * DRIFT_WANDER * drift_amt;
                v.penv *= penv_coef;
                let (mut sl, mut sr_) = (0.0, 0.0);
                for ((((o1, o2), o3), c), sc) in v
                    .osc1
                    .iter_mut()
                    .zip(v.osc2.iter_mut())
                    .zip(v.osc3.iter_mut())
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
                    if l3 > 0.0 {
                        let hz3 = 440.0 * ((p + off + osc3_offset - 69.0) / 12.0).exp2();
                        s += o3.next_sample(w1, hz3 / sr) * l3;
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
                if sub_level > 0.0 {
                    // A sine an octave down, centred (one per voice, not per unison copy).
                    v.sub = (v.sub + 440.0 * ((p - 81.0) / 12.0).exp2() / sr).fract();
                    let x = (v.sub * std::f32::consts::TAU).sin() * sub_level;
                    sl += x;
                    sr_ += x;
                }
                let fe = v.fenv.next_value();
                if update {
                    let cut = base_cut * (fe * env_amt + lfo * lfo_cut).exp2();
                    v.filter.set(cut, reso, sr);
                    v.filter_r.set(cut, reso, sr);
                    if steep {
                        // The second stage is not resonant (a 24 dB slope, one peak).
                        v.filter2.set(cut, 0.0, sr);
                        v.filter2_r.set(cut, 0.0, sr);
                    }
                }
                let a = v.amp.next_value();
                let mut yl = v.filter.process(sl, mode);
                if steep {
                    yl = v.filter2.process(yl, mode);
                }
                let yl = yl * a * v.vel_gain;
                if stereo {
                    let mut yr = v.filter_r.process(sr_, mode);
                    if steep {
                        yr = v.filter2_r.process(yr, mode);
                    }
                    out_l += yl;
                    out_r += yr * a * v.vel_gain;
                } else {
                    // Keep the right filters in step so turning the width up mid-note
                    // does not click.
                    v.filter_r = v.filter.clone();
                    v.filter2_r = v.filter2.clone();
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
            let mut side = side;
            // Width without detuning: the delayed high band of the mid goes to the side
            // (cancels in mono; the mono-bass stage below keeps the bass centred).
            if width > 0.0 {
                // Two one-pole high-passes in a row (12 dB/oct).
                let [a, b] = &mut self.wide_low;
                *a += (mid - *a) * wide_k;
                let high = mid - *a;
                *b += (high - *b) * wide_k;
                if let Some(slot) = self.wide_buf.get_mut(self.wide_pos) {
                    *slot = high - *b;
                }
                let read = (self.wide_pos + WIDTH_BUF - wide_delay) % WIDTH_BUF;
                side += self.wide_buf.get(read).copied().unwrap_or(0.0) * width;
                self.wide_pos = (self.wide_pos + 1) % WIDTH_BUF;
            }
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

/// A quick setting: (button, tooltip, parameter values it sets).
pub type QuickSetting = (&'static str, &'static str, &'static [(u64, f32)]);

/// Quick settings for the device panel: (button, tooltip, values); other knobs stay.
pub const QUICK: &[QuickSetting] = &[(
    "Pure",
    "Just intonation without beating: pure-tuned chords, no detune, drift, unison or \
     pitch wobble, filter fully open (the Raw stab sound)",
    &[
        (id::JUST, 1.0),
        (id::JUST_KEYS, 2.0),
        (id::OSC2_DETUNE, 0.0),
        (id::UNISON, 1.0),
        (id::DRIFT, 0.0),
        (id::LFO_PITCH, 0.0),
        (id::FILTER_MODE, 1.0),
        (id::CUTOFF, 20.0),
        (id::RESONANCE, 0.0),
        (id::FILTER_ENV, 0.0),
    ],
)];

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
        // Measured from Aloboi's "Just Raw" videos: plain, unfiltered saws in just
        // intonation, steady, barely saturated; play root + fifth + tenth (C2 G2 E3).
        "Raw stab",
        &[
            (id::OSC1_LEVEL, 0.9),
            (id::OSC2_LEVEL, 0.0),
            (id::FILTER_MODE, 1.0),
            (id::CUTOFF, 40.0),
            (id::RESONANCE, 0.0),
            (id::FILTER_ENV, 0.0),
            (id::AMP_A, 0.002),
            (id::AMP_D, 0.1),
            (id::AMP_S, 1.0),
            (id::AMP_R, 0.08),
            (id::DRIVE, 0.1),
            (id::DRIVE_TYPE, 2.0),
            (id::DRIFT, 0.0),
            (id::JUST, 1.0),
            (id::JUST_KEYS, 2.0),
            // Measured on the owner's reference: overtone-series chords (27/16, 7/4 …),
            // dry and centred (the width comes from the reverb).
            (id::TUNING, 1.0),
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
    // Measured from the owner's EDM songs (2026-11-26).
    (
        // "Turn Up The Bass" intro riff: saws stacked at 0 / +12 / +19, no detune, gated.
        "Big room riff",
        &[
            (id::OSC2_WAVE, 0.0),
            (id::OSC2_LEVEL, 0.7),
            (id::OSC2_SEMI, 12.0),
            (id::OSC2_DETUNE, 0.0),
            (id::OSC3_LEVEL, 0.45),
            (id::OSC3_SEMI, 19.0),
            (id::CUTOFF, 5_000.0),
            (id::RESONANCE, 0.1),
            (id::FILTER_ENV, 0.0),
            (id::AMP_A, 0.015),
            (id::AMP_D, 0.1),
            (id::AMP_S, 1.0),
            (id::AMP_R, 0.02),
            (id::GLIDE, 0.05),
            (id::DRIFT, 0.0),
        ],
    ),
    (
        // "Fly High": 9 saws spread about 65 cents, full width, no filter.
        "Euphoric supersaw",
        &[
            (id::OSC2_WAVE, 0.0),
            (id::OSC2_LEVEL, 0.4),
            (id::OSC2_SEMI, 12.0),
            (id::OSC2_DETUNE, 0.0),
            (id::UNISON, 9.0),
            (id::UNISON_DETUNE, 65.0),
            (id::UNISON_SPREAD, 1.0),
            (id::CUTOFF, 20_000.0),
            (id::FILTER_ENV, 0.0),
            (id::AMP_A, 0.01),
            (id::AMP_D, 0.3),
            (id::AMP_S, 0.9),
            (id::AMP_R, 0.3),
            (id::DRIFT, 0.2),
        ],
    ),
    (
        // "Dramma": rolling 16th bass, one saw through a 24 dB low-pass near 350 Hz.
        "Rolling bass",
        &[
            (id::OSC2_LEVEL, 0.0),
            (id::CUTOFF, 350.0),
            (id::FILTER_SLOPE, 1.0),
            (id::RESONANCE, 0.1),
            (id::FILTER_ENV, 0.3),
            (id::FILT_A, 0.001),
            (id::FILT_D, 0.08),
            (id::FILT_S, 0.2),
            (id::AMP_A, 0.003),
            (id::AMP_D, 0.1),
            (id::AMP_S, 0.7),
            (id::AMP_R, 0.03),
            (id::DRIFT, 0.0),
        ],
    ),
    (
        // "HEADSHOT" intro: dark pad whose whole chord glides into the next, light vibrato.
        "Gliding pad",
        &[
            (id::OSC2_LEVEL, 0.0),
            (id::CUTOFF, 500.0),
            (id::FILTER_SLOPE, 1.0),
            (id::RESONANCE, 0.1),
            (id::FILTER_ENV, 0.0),
            (id::GLIDE, 0.4),
            (id::GLIDE_MODE, 1.0),
            (id::LFO_RATE, 3.7),
            (id::LFO_PITCH, 0.05),
            (id::AMP_A, 0.05),
            (id::AMP_D, 0.5),
            (id::AMP_S, 1.0),
            (id::AMP_R, 0.6),
            (id::DRIFT, 0.0),
        ],
    ),
    (
        // "Friday I'm In Luv" / "HEADSHOT" sub: a sine with a little saturation.
        "Sine sub",
        &[
            (id::OSC1_WAVE, 3.0),
            (id::OSC2_LEVEL, 0.0),
            (id::CUTOFF, 20_000.0),
            (id::FILTER_ENV, 0.0),
            (id::AMP_A, 0.005),
            (id::AMP_D, 0.3),
            (id::AMP_S, 1.0),
            (id::AMP_R, 0.05),
            (id::DRIVE, 0.25),
            (id::DRIVE_TYPE, 0.0),
            (id::VELOCITY, 0.3),
            (id::DRIFT, 0.0),
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
    fn the_pure_quick_setting_removes_beating() {
        let base = [
            (id::OSC2_LEVEL, 0.7),
            (id::OSC2_DETUNE, 12.0),
            (id::UNISON, 3.0),
            (id::DRIFT, 0.5),
            (id::AMP_A, 0.002),
            (id::AMP_S, 1.0),
        ];
        // Loudness swing of a held C major chord after the attack, in windows of four
        // periods of the pure chord (4:5:6 over C3 / 4 = 32.7 Hz).
        let swing = |settings: &[(u64, f32)]| {
            let mut s = synth(settings);
            let out = render(&mut s, &[on(0, 48), on(0, 52), on(0, 55)], 72_000);
            let w: Vec<f32> = out[12_000..].chunks_exact(5_871).map(rms).collect();
            let (lo, hi) = w
                .iter()
                .fold((f32::MAX, 0.0_f32), |(l, h), &x| (l.min(x), h.max(x)));
            (hi - lo) / hi
        };
        let pure: Vec<(u64, f32)> = base.iter().chain(QUICK[0].2).copied().collect();
        let (before, after) = (swing(&base), swing(&pure));
        assert!(before > 0.2, "{before}");
        assert!(after < 0.01, "{after}");
    }

    #[test]
    fn just_offsets_are_pure_intervals() {
        assert!(just_offset(0, false).abs() < 1e-6);
        assert!((just_offset(7, false) - 0.019_55).abs() < 1e-4, "fifth");
        assert!(
            (just_offset(4, false) + 0.136_86).abs() < 1e-4,
            "major third"
        );
        assert!(
            (just_offset(19, false) - just_offset(7, false)).abs() < 1e-6,
            "octaves"
        );
        assert!(
            (just_offset(-5, false) - just_offset(7, false)).abs() < 1e-6,
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

    /// A pure sine synth with just intonation and no drift or filter.
    fn sine_just(extra: &[(u64, f32)]) -> Polysynth {
        let mut settings = vec![
            (id::OSC1_WAVE, 3.0),
            (id::CUTOFF, 20_000.0),
            (id::FILTER_ENV, 0.0),
            (id::DRIFT, 0.0),
            (id::JUST, 1.0),
        ];
        settings.extend_from_slice(extra);
        synth(&settings)
    }

    fn just_of(s: &Polysynth, key: u8) -> (f32, f32) {
        s.voices
            .iter()
            .find(|v| v.key == Some(key))
            .map_or((f32::NAN, f32::NAN), |v| (v.just, v.just_now))
    }

    #[test]
    fn a_chord_is_in_tune_at_once_whatever_order_its_notes_arrive() {
        let mut s = sine_just(&[]);
        // Third and fifth before the root, at the same moment.
        render(&mut s, &[on(0, 64), on(0, 67), on(0, 60)], 64);
        let (want, now) = just_of(&s, 64);
        assert!((want + 0.136_86).abs() < 1e-4, "E over C: {want}");
        assert!(
            (now - want).abs() < 1e-4,
            "no slide at the chord start: {now}"
        );
        // A later note slides into the new tuning instead of jumping.
        render(&mut s, &[], 4_800);
        render(&mut s, &[on(0, 57)], 1);
        let (want, now) = just_of(&s, 64);
        assert!((now - want).abs() > 1e-3, "slides after the chord started");
    }

    #[test]
    fn the_root_can_stay_while_few_keys_are_held_or_follow_the_most_played_note() {
        let note_off = |key| ev(0, EventKind::NoteOff { ch: 0, key });
        // Retune only from 3 keys: a melody note after the chord keeps the chord's root.
        let mut s = sine_just(&[(id::JUST_KEYS, 3.0)]);
        render(&mut s, &[on(0, 48), on(0, 52), on(0, 55)], 64);
        render(
            &mut s,
            &[note_off(48), note_off(52), note_off(55), on(1, 57)],
            64,
        );
        let (a, _) = just_of(&s, 57);
        assert!((a + 0.156_4).abs() < 1e-3, "A tuned as 5:3 over C: {a}");
        // Most played: G played often is the root of a C–G dyad.
        let mut s = sine_just(&[(id::JUST_ROOT, 1.0)]);
        for _ in 0..3 {
            render(&mut s, &[on(0, 55)], 64);
            render(&mut s, &[note_off(55)], 64);
        }
        render(&mut s, &[on(0, 48), on(0, 55)], 64);
        let (g, _) = just_of(&s, 55);
        let (c, _) = just_of(&s, 48);
        assert!(g.abs() < 1e-6, "G is the root: {g}");
        assert!((c + 0.019_55).abs() < 1e-4, "C a pure fourth below: {c}");
    }

    #[test]
    fn overtone_tuning_uses_the_harmonic_series() {
        assert!((just_offset(10, true) + 0.311_7).abs() < 1e-3, "7:4");
        assert!((just_offset(6, true) + 0.486_8).abs() < 1e-3, "11:8");
        assert!((just_offset(8, true) - 0.405_2).abs() < 1e-3, "13:8");
    }

    #[test]
    fn pitch_bend_moves_the_chord_or_one_note_of_it() {
        let bend_up = cc(0, crate::controllers::cc::PITCH_BEND, 1.0);
        // C4 + G4, the second note bent up a whole tone to A4.
        let mut s = sine_just(&[(id::BEND_NOTE, 2.0), (id::BEND_RANGE, 2.0)]);
        let x = render(&mut s, &[bend_up, on(0, 60), on(0, 67)], 24_000);
        let m = |hz| magnitude(&x[4_800..], hz);
        assert!(m(441.0) > 10.0 * m(392.4), "G bent to A");
        assert!(m(261.6) > 10.0 * m(293.7), "C stays");
        // Bend note 0: the whole chord moves.
        let mut s = sine_just(&[(id::BEND_RANGE, 2.0)]);
        let x = render(&mut s, &[bend_up, on(0, 60)], 24_000);
        assert!(magnitude(&x[4_800..], 293.7) > 10.0 * magnitude(&x[4_800..], 261.6));
    }

    #[test]
    fn the_bend_note_can_count_from_the_top() {
        let bend_up = cc(0, crate::controllers::cc::PITCH_BEND, 1.0);
        // C4 + E4 + G4, the top note bent up a whole tone to A4.
        let mut s = sine_just(&[
            (id::BEND_NOTE, 1.0),
            (id::BEND_FROM, 1.0),
            (id::BEND_RANGE, 2.0),
        ]);
        let x = render(&mut s, &[bend_up, on(0, 60), on(0, 64), on(0, 67)], 24_000);
        let m = |hz| magnitude(&x[4_800..], hz);
        assert!(m(441.0) > 10.0 * m(392.4), "G bent to A");
        assert!(m(261.6) > 10.0 * m(293.7), "C stays");
    }

    #[test]
    fn a_fixed_root_key_turns_dynamic_retuning_off() {
        // Root D: C is a minor seventh above it (7/4), E a major second (9/8).
        let mut s = sine_just(&[(id::JUST_ROOT, 2.0), (id::JUST_KEY, 2.0)]);
        render(&mut s, &[on(0, 60), on(0, 64)], 64);
        let cents = |r: f32, semis: f32| 12.0 * r.log2() - semis;
        assert!((just_of(&s, 60).0 - cents(7.0 / 4.0, 10.0)).abs() < 1e-4);
        assert!((just_of(&s, 64).0 - cents(9.0 / 8.0, 2.0)).abs() < 1e-4);
    }

    #[test]
    fn each_interval_can_take_its_own_ratio() {
        // A3 + C4: the minor third as 19/16 instead of 6/5.
        let mut s = sine_just(&[(id::INTERVAL_M2 + 2, 2.0)]);
        render(&mut s, &[on(0, 57), on(0, 60)], 64);
        let want = 12.0 * (19.0_f32 / 16.0).log2() - 3.0;
        assert!((just_of(&s, 60).0 - want).abs() < 1e-4);
        assert!(just_of(&s, 57).0.abs() < 1e-6, "the root stays");
        // "Table" keeps the tuning table's ratio.
        let mut s = sine_just(&[]);
        render(&mut s, &[on(0, 57), on(0, 60)], 64);
        assert!((just_of(&s, 60).0 - just_offset(3, false)).abs() < 1e-6);
        assert_eq!(
            INTERVAL_NAMES.map(|n| n.len()),
            INTERVAL_RATIOS.map(|r| r.len() + 1)
        );
    }

    #[test]
    fn high_width_is_mono_compatible_and_leaves_the_bass_centred() {
        let play = |width: f32| {
            render_stereo(
                &mut synth(&[(id::CUTOFF, 20_000.0), (id::DRIFT, 0.0), (id::WIDTH, width)]),
                &[on(0, 48)],
                24_000,
            )
        };
        let (narrow, wide) = (play(0.0), play(1.0));
        let side =
            |x: &[Vec<f32>]| -> Vec<f32> { x[0].iter().zip(&x[1]).map(|(l, r)| l - r).collect() };
        let sum =
            |x: &[Vec<f32>]| -> Vec<f32> { x[0].iter().zip(&x[1]).map(|(l, r)| l + r).collect() };
        assert!(rms(&side(&narrow)) < 1e-6);
        assert!(
            rms(&side(&wide)[4_800..]) > 0.05 * rms(&sum(&wide)[4_800..]),
            "wider"
        );
        let diff: Vec<f32> = sum(&wide)
            .iter()
            .zip(sum(&narrow))
            .map(|(a, b)| a - b)
            .collect();
        assert!(rms(&diff) < 1e-5, "the mono sum is unchanged");
        let s = side(&wide);
        let mid_bass = magnitude(&sum(&wide)[4_800..], 130.8);
        let side_bass = magnitude(&s[4_800..], 130.8);
        assert!(
            side_bass < 0.01 * mid_bass,
            "bass centred: {}",
            side_bass / mid_bass
        );
    }

    #[test]
    fn just_intonation_starts_every_stab_the_same() {
        let note_off = |key| ev(0, EventKind::NoteOff { ch: 0, key });
        let mut s = synth(&[(id::JUST, 1.0), (id::DRIFT, 0.0), (id::AMP_R, 0.01)]);
        let first = render(&mut s, &[on(0, 48), on(0, 55)], 4_800);
        render(&mut s, &[note_off(48), note_off(55)], 9_600);
        let second = render(&mut s, &[on(0, 48), on(0, 55)], 4_800);
        let diff = first
            .iter()
            .zip(&second)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(diff < 0.02, "{diff}");
    }

    #[test]
    fn osc_3_sub_and_the_24_db_slope() {
        let sine = |extra: &[(u64, f32)]| {
            let mut settings = vec![
                (id::OSC1_WAVE, 3.0),
                (id::CUTOFF, 20_000.0),
                (id::FILTER_ENV, 0.0),
                (id::DRIFT, 0.0),
            ];
            settings.extend_from_slice(extra);
            render(&mut synth(&settings), &[on(0, 57)], 24_000)
        };
        // A3 = 220 Hz; osc 3 at +19 st = 659.3 Hz; the sub at 110 Hz.
        let x = sine(&[(id::OSC3_LEVEL, 1.0), (id::SUB, 1.0)]);
        let m = |hz| magnitude(&x[4_800..], hz);
        assert!(m(659.3) > 0.5 * m(220.0), "osc 3");
        assert!(m(110.0) > 0.5 * m(220.0), "sub");
        let plain = sine(&[]);
        assert!(magnitude(&plain[4_800..], 110.0) < 0.01 * magnitude(&plain[4_800..], 220.0));
        // A saw through a low-pass at 500 Hz: the 24 dB slope takes 2 kHz much further down.
        let saw = |slope: f32| {
            render(
                &mut synth(&[
                    (id::CUTOFF, 500.0),
                    (id::RESONANCE, 0.0),
                    (id::FILTER_ENV, 0.0),
                    (id::DRIFT, 0.0),
                    (id::FILTER_SLOPE, slope),
                ]),
                &[on(0, 45)],
                24_000,
            )
        };
        let ratio = |x: &[f32]| magnitude(&x[4_800..], 1_980.0) / magnitude(&x[4_800..], 110.0);
        let (gentle, steep) = (ratio(&saw(0.0)), ratio(&saw(1.0)));
        assert!(steep < 0.3 * gentle, "24 dB: {steep} vs 12 dB: {gentle}");
    }

    #[test]
    fn pitch_envelope_starts_high_and_falls() {
        let x = render(
            &mut synth(&[
                (id::OSC1_WAVE, 3.0),
                (id::CUTOFF, 20_000.0),
                (id::DRIFT, 0.0),
                (id::PITCH_ENV, 12.0),
                (id::PITCH_DECAY, 0.02),
            ]),
            &[on(0, 57)],
            24_000,
        );
        // Zero crossings per 10 ms: about twice as many at the start as later.
        let crossings = |a: usize| {
            x[a..a + 480]
                .windows(2)
                .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
                .count()
        };
        assert!(crossings(48) >= 3, "starts an octave up: {}", crossings(48));
        assert!(
            (2..=3).contains(&crossings(12_000)),
            "ends at 220 Hz: {}",
            crossings(12_000)
        );
    }

    #[test]
    fn each_voice_glide_slides_the_chord_into_the_next() {
        let off = |key| ev(0, EventKind::NoteOff { ch: 0, key });
        let mut s = synth(&[
            (id::OSC1_WAVE, 3.0),
            (id::DRIFT, 0.0),
            (id::GLIDE, 0.3),
            (id::GLIDE_MODE, 1.0),
            (id::AMP_R, 1.0),
        ]);
        render(&mut s, &[on(0, 60), on(0, 64)], 9_600);
        // C+E → D+G: the lower voice slides from C, the upper from E (not both from E).
        render(&mut s, &[off(60), off(64), on(0, 62), on(0, 67)], 480);
        let mut pitches: Vec<f32> = s
            .voices
            .iter()
            .filter(|v| v.key.is_some())
            .map(|v| v.pitch)
            .collect();
        pitches.sort_by(f32::total_cmp);
        assert_eq!(pitches.len(), 2);
        assert!(
            pitches[0] < 61.5 && pitches[0] > 60.0,
            "from C: {}",
            pitches[0]
        );
        assert!(
            pitches[1] < 66.5 && pitches[1] > 64.0,
            "from E: {}",
            pitches[1]
        );
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
