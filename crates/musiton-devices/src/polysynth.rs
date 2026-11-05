//! Polysynth: 2 oscillators + noise → state-variable filter with envelope → amp ADSR,
//! LFO on pitch and cutoff, glide, 16 voices with voice stealing.

use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_dsp::env::Adsr;
use musiton_dsp::filter::{FilterMode, Svf};
use musiton_dsp::osc::{BlepOsc, Noise, Wave};
use musiton_dsp::smooth::LinearRamp;
use musiton_engine::node::{AudioIo, AudioNode, Event, EventKind, ProcessContext};

/// Number of voices.
pub const VOICES: usize = 16;
/// Filter coefficients are recomputed every this many samples.
const FILTER_UPDATE: usize = 8;
/// Output headroom.
const HEADROOM: f32 = 0.25;

const WAVES: &[&str] = &["Saw", "Square", "Triangle", "Sine"];
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
}

/// All parameters (index = id).
pub const PARAMS: [ParamInfo; 25] = [
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
];

#[derive(Debug, Clone, Default)]
struct Voice {
    key: Option<u8>,
    osc1: BlepOsc,
    osc2: BlepOsc,
    filter: Svf,
    amp: Adsr,
    fenv: Adsr,
    pitch: f32,
    target: f32,
    vel_gain: f32,
    age: u64,
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
        v.amp.gate_on();
        v.fenv.gate_on();
        self.last_pitch = Some(p);
    }

    fn note_off(&mut self, key: u8) {
        for v in self.voices.iter_mut().filter(|v| v.key == Some(key)) {
            v.key = None;
            v.amp.gate_off();
            v.fenv.gate_off();
        }
    }

    fn render(&mut self, io: &mut AudioIo, from: usize, to: usize) {
        if self.dirty {
            self.update_envelopes();
        }
        let sr = self.sample_rate;
        let w1 = Wave::from_index(self.v(id::OSC1_WAVE));
        let w2 = Wave::from_index(self.v(id::OSC2_WAVE));
        let (l1, l2, nl) = (
            self.v(id::OSC1_LEVEL),
            self.v(id::OSC2_LEVEL),
            self.v(id::NOISE),
        );
        let osc2_offset = self.v(id::OSC2_SEMI).round() + self.v(id::OSC2_DETUNE) / 100.0;
        let mode = FilterMode::from_index(self.v(id::FILTER_MODE));
        let reso = self.v(id::RESONANCE);
        let env_amt = self.v(id::FILTER_ENV) * 6.0;
        let lfo_inc = self.v(id::LFO_RATE) / sr;
        let (lfo_pitch, lfo_cut) = (self.v(id::LFO_PITCH), self.v(id::LFO_CUTOFF) * 4.0);
        let glide = self.v(id::GLIDE);
        let glide_coef = if glide > 0.0 {
            (-4.6 / (glide * sr)).exp()
        } else {
            0.0
        };
        self.volume.set_target(self.v(id::VOLUME));
        self.cutoff.set_target(self.v(id::CUTOFF));

        for i in from..to {
            let lfo = (self.lfo_phase * std::f32::consts::TAU).sin();
            self.lfo_phase = (self.lfo_phase + lfo_inc).fract();
            let base_cut = self.cutoff.next_value();
            let vol = self.volume.next_value() * HEADROOM;
            let update = self.counter.is_multiple_of(FILTER_UPDATE as u64);
            self.counter += 1;
            let mut out = 0.0;
            for v in &mut self.voices {
                if !v.amp.is_active() {
                    continue;
                }
                v.pitch = v.target + (v.pitch - v.target) * glide_coef;
                let p = v.pitch + lfo * lfo_pitch;
                let hz = 440.0 * ((p - 69.0) / 12.0).exp2();
                let hz2 = 440.0 * ((p + osc2_offset - 69.0) / 12.0).exp2();
                let mut s = v.osc1.next_sample(w1, hz / sr) * l1;
                if l2 > 0.0 {
                    s += v.osc2.next_sample(w2, hz2 / sr) * l2;
                }
                if nl > 0.0 {
                    s += self.noise.next_sample() * nl;
                }
                let fe = v.fenv.next_value();
                if update {
                    let cut = base_cut * (fe * env_amt + lfo * lfo_cut).exp2();
                    v.filter.set(cut, reso, sr);
                }
                let a = v.amp.next_value();
                out += v.filter.process(s, mode) * a * v.vel_gain;
            }
            let y = out * vol;
            for ch in 0..2 {
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
        self.voices = vec![Voice::default(); VOICES];
        self.volume = LinearRamp::with_time(self.v(id::VOLUME), 20.0, sample_rate);
        self.cutoff = LinearRamp::with_time(self.v(id::CUTOFF), 20.0, sample_rate);
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
                    for k in 0..128 {
                        self.note_off(k);
                    }
                }
                EventKind::Param { id, val } => self.set_param(id, val),
                EventKind::Cc { .. } => {}
            }
        }
        self.render(io, pos, n);
    }

    fn reset(&mut self) {
        for v in &mut self.voices {
            *v = Voice::default();
        }
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
];

#[cfg(test)]
mod tests {
    use super::*;
    use musiton_core::{TempoMap, TimeSig};

    fn render(s: &mut Polysynth, events: &[Event], frames: usize) -> Vec<f32> {
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
        outs.swap_remove(0)
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
