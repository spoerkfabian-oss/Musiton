//! Acid bass: one voice (saw or square) through a resonant 24 dB ladder filter with a
//! decaying envelope. Overlapping notes slide in pitch without a new attack; loud notes
//! (velocity ≥ 0.75) are accented: louder, with more filter sweep.

use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_dsp::filter::Ladder;
use musiton_dsp::osc::{BlepOsc, Wave};
use musiton_engine::node::{AudioIo, AudioNode, Event, EventKind, ProcessContext};

use crate::controllers::{Change, Controllers};
use crate::voices::{note_hz, split_block, Params};

/// Output headroom.
const HEADROOM: f32 = 0.5;
/// Velocity from which a note is accented.
const ACCENT_VELOCITY: f32 = 0.75;
/// Filter coefficients are recomputed every this many samples.
const FILTER_UPDATE: u32 = 4;
/// Most keys remembered for returning to a held note.
const STACK: usize = 16;

const WAVES: &[&str] = &["Saw", "Square"];

/// Parameter ids.
#[allow(missing_docs)]
pub mod id {
    pub const WAVE: u64 = 0;
    pub const TUNE: u64 = 1;
    pub const CUTOFF: u64 = 2;
    pub const RESONANCE: u64 = 3;
    pub const ENV_MOD: u64 = 4;
    pub const DECAY: u64 = 5;
    pub const ACCENT: u64 = 6;
    pub const SLIDE: u64 = 7;
    pub const DRIVE: u64 = 8;
    pub const VOLUME: u64 = 9;
}

/// All parameters (index = id).
pub const PARAMS: [ParamInfo; 10] = [
    ParamInfo::choice(id::WAVE, "Wave", WAVES, 0),
    ParamInfo::new(id::TUNE, "Tune", -12.0, 12.0, 0.0).unit("st"),
    ParamInfo::new(id::CUTOFF, "Cutoff", 40.0, 8_000.0, 500.0)
        .log()
        .unit("Hz"),
    ParamInfo::new(id::RESONANCE, "Resonance", 0.0, 0.95, 0.6),
    ParamInfo::new(id::ENV_MOD, "Env mod", 0.0, 1.0, 0.5),
    ParamInfo::new(id::DECAY, "Decay", 0.05, 2.0, 0.3)
        .log()
        .unit("s"),
    ParamInfo::new(id::ACCENT, "Accent", 0.0, 1.0, 0.5),
    ParamInfo::new(id::SLIDE, "Slide", 0.01, 0.5, 0.06)
        .log()
        .unit("s"),
    ParamInfo::new(id::DRIVE, "Drive", 0.0, 1.0, 0.2),
    ParamInfo::new(id::VOLUME, "Volume", 0.0, 1.0, 0.7),
];

/// The acid bass node (no inputs, stereo out).
#[derive(Debug)]
pub struct Acid {
    p: Params<10>,
    controllers: Controllers,
    sample_rate: f32,
    osc: BlepOsc,
    filter: Ladder,
    /// Held keys, most recent last.
    stack: [u8; STACK],
    held: usize,
    pitch: f32,
    target: f32,
    /// Amplitude (follows the gate with a few milliseconds of attack and release).
    amp: f32,
    gate: bool,
    /// Filter envelope (1 at a new note, decaying).
    fenv: f32,
    accent: bool,
    counter: u32,
}

impl Default for Acid {
    fn default() -> Self {
        Self {
            p: Params::new(&PARAMS),
            controllers: Controllers::default(),
            sample_rate: 48_000.0,
            osc: BlepOsc::default(),
            filter: Ladder::default(),
            stack: [0; STACK],
            held: 0,
            pitch: 36.0,
            target: 36.0,
            amp: 0.0,
            gate: false,
            fenv: 0.0,
            accent: false,
            counter: 0,
        }
    }
}

impl Acid {
    fn note_on(&mut self, key: u8, vel: f32) {
        let legato = self.held > 0 && self.gate;
        if self.held == STACK {
            self.stack.copy_within(1.., 0);
            self.held -= 1;
        }
        if let Some(slot) = self.stack.get_mut(self.held) {
            *slot = key;
            self.held += 1;
        }
        self.target = f32::from(key);
        if !legato {
            // A new phrase: jump to the pitch and restart the envelopes.
            self.pitch = self.target;
            self.fenv = 1.0;
            self.accent = vel >= ACCENT_VELOCITY;
        }
        self.gate = true;
    }

    fn note_off(&mut self, key: u8) {
        if self.controllers.sustain {
            return;
        }
        self.remove(key);
    }

    fn remove(&mut self, key: u8) {
        let Some(i) = self.stack.iter().take(self.held).position(|&k| k == key) else {
            return;
        };
        self.stack.copy_within(i + 1..self.held, i);
        self.held -= 1;
        match self.held.checked_sub(1).and_then(|i| self.stack.get(i)) {
            // Back to the key still held: slide there.
            Some(&k) => self.target = f32::from(k),
            None => self.gate = false,
        }
    }

    fn release_all(&mut self) {
        self.held = 0;
        self.gate = false;
    }

    fn render(&mut self, io: &mut AudioIo, from: usize, to: usize) {
        let sr = self.sample_rate;
        let wave = if self.p.index(id::WAVE) == 1 {
            Wave::Square
        } else {
            Wave::Saw
        };
        let gain = self.controllers.gain();
        let attack = 1.0 - (-1.0 / (0.002 * sr)).exp();
        let release = 1.0 - (-1.0 / (0.008 * sr)).exp();
        for i in from..to {
            self.p.tick();
            let p = &self.p;
            let slide = (-4.6 / (p.get(id::SLIDE) * sr)).exp();
            self.pitch = self.target + (self.pitch - self.target) * slide;
            let decay = (-4.6 / (p.get(id::DECAY) * sr)).exp();
            self.fenv *= decay;
            let accent = if self.accent { p.get(id::ACCENT) } else { 0.0 };
            if self.counter.is_multiple_of(FILTER_UPDATE) {
                let sweep = p.get(id::ENV_MOD) * (1.0 + accent) * self.fenv;
                let cutoff = p.get(id::CUTOFF) * (sweep * 5.0).exp2();
                self.filter
                    .set(cutoff, p.get(id::RESONANCE) * (1.0 - 0.3 * accent), sr);
            }
            self.counter = self.counter.wrapping_add(1);
            let target_amp = if self.gate { 1.0 } else { 0.0 };
            let coef = if self.gate { attack } else { release };
            self.amp += (target_amp - self.amp) * coef;
            let hz = note_hz(self.pitch + p.get(id::TUNE));
            let x = self.osc.next_sample(wave, hz / sr);
            let drive = 1.0 + 6.0 * p.get(id::DRIVE);
            let y = self.filter.process(x);
            let shaped = (y * drive).tanh() / drive.sqrt();
            let level = p.get(id::VOLUME) * gain * HEADROOM * (1.0 + 0.6 * accent);
            let s = shaped * self.amp * level;
            for ch in 0..2 {
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = s;
                }
            }
        }
    }
}

impl AudioNode for Acid {
    fn num_inputs(&self) -> usize {
        0
    }

    fn num_outputs(&self) -> usize {
        2
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        self.p.prepare(sample_rate);
    }

    fn process(&mut self, _ctx: &ProcessContext, io: &mut AudioIo, events: &[Event]) {
        let n = io.frames();
        split_block(
            self,
            n,
            events,
            |s, a, b| s.render(io, a, b),
            |s, e| match *e {
                EventKind::NoteOn { key, vel, .. } => s.note_on(key, vel),
                EventKind::NoteOff { key, .. } => s.note_off(key),
                EventKind::AllNotesOff => {
                    s.controllers.sustain = false;
                    s.release_all();
                }
                EventKind::Param { id, val } => s.p.set(id, val),
                EventKind::Cc { cc, val, .. } => {
                    if s.controllers.set(cc, val) == Change::PedalUp {
                        // Keys released while the pedal was down are let go now.
                        s.release_all();
                    }
                }
            },
        );
    }

    fn reset(&mut self) {
        self.release_all();
        self.amp = 0.0;
        self.fenv = 0.0;
        self.filter.reset();
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
        "Squelch",
        &[
            (id::CUTOFF, 300.0),
            (id::RESONANCE, 0.85),
            (id::ENV_MOD, 0.8),
            (id::DECAY, 0.2),
            (id::ACCENT, 0.8),
            (id::DRIVE, 0.5),
        ],
    ),
    (
        "Deep square",
        &[
            (id::WAVE, 1.0),
            (id::CUTOFF, 250.0),
            (id::RESONANCE, 0.4),
            (id::ENV_MOD, 0.3),
            (id::DECAY, 0.5),
            (id::DRIVE, 0.1),
        ],
    ),
    (
        "Screamer",
        &[
            (id::CUTOFF, 900.0),
            (id::RESONANCE, 0.92),
            (id::ENV_MOD, 0.6),
            (id::DECAY, 0.12),
            (id::DRIVE, 1.0),
            (id::VOLUME, 0.5),
        ],
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use musiton_core::{TempoMap, TimeSig};

    fn render(events: &[Event], frames: usize) -> Vec<f32> {
        let mut s = Acid::default();
        s.prepare(48_000.0, frames);
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

    fn ev(offset: u32, kind: EventKind) -> Event {
        Event { offset, kind }
    }

    fn period(x: &[f32]) -> f32 {
        let zc: Vec<usize> = (1..x.len())
            .filter(|&i| x[i - 1] < 0.0 && x[i] >= 0.0)
            .collect();
        (zc[zc.len() - 1] - zc[0]) as f32 / (zc.len() - 1) as f32
    }

    #[test]
    fn overlapping_notes_slide_and_return_to_the_held_key() {
        let on = |o, key| {
            ev(
                o,
                EventKind::NoteOn {
                    ch: 0,
                    key,
                    vel: 0.5,
                },
            )
        };
        let off = |o, key| ev(o, EventKind::NoteOff { ch: 0, key });
        // 45 (110 Hz) held, 57 (220 Hz) on top, then released again.
        let out = render(&[on(0, 45), on(24_000, 57), off(48_000, 57)], 72_000);
        let p1 = period(&out[12_000..23_000]);
        let p2 = period(&out[36_000..47_000]);
        let p3 = period(&out[60_000..71_000]);
        assert!((p1 - 436.4).abs() < 3.0, "{p1}");
        assert!((p2 - 218.2).abs() < 2.0, "{p2}");
        assert!((p3 - 436.4).abs() < 3.0, "{p3}");
        // No new attack: the level does not dip at the slide.
        let level = |r: std::ops::Range<usize>| out[r].iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!(level(23_900..24_300) > 0.3 * level(20_000..23_900));
    }
}
