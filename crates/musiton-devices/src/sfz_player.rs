//! Sample instrument: plays multi-sampled instruments described by SFZ files (what the free
//! orchestral, piano and choir libraries use). Each note picks the regions whose key and
//! velocity ranges match, steps through round-robin variations, can trigger release
//! samples on note-off, loops, and lets one group of sounds cut another (`off_by`).
//!
//! The SFZ file is read and its samples are decoded off the audio thread (by the app); this
//! node gets the finished [`SfzRegion`] list. Samples are read with cubic interpolation
//! (regions usually cover a few keys each, so no mipmaps are needed, which keeps big
//! libraries at their plain size in memory).

use std::sync::Arc;

use musiton_core::audio::AudioData;
use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_dsp::env::Adsr;
use musiton_dsp::resample::cubic;
use musiton_engine::node::{AudioIo, AudioNode, Event, EventKind, ProcessContext};

use crate::controllers::{Change, Controllers};

/// Number of voices (orchestral parts with release samples need many).
pub const VOICES: usize = 64;
/// Fade (seconds) of a voice cut by `off_by` or stolen.
const CUT_FADE: f32 = 0.01;

/// Parameter ids.
#[allow(missing_docs)]
pub mod id {
    pub const VOLUME: u64 = 0;
    pub const TUNE: u64 = 1;
    pub const ATTACK: u64 = 2;
    pub const RELEASE: u64 = 3;
}

/// All parameters (index = id).
pub const PARAMS: [ParamInfo; 4] = [
    ParamInfo::new(id::VOLUME, "Volume", 0.0, 1.0, 0.8),
    ParamInfo::new(id::TUNE, "Tune", -24.0, 24.0, 0.0).unit("st"),
    ParamInfo::new(id::ATTACK, "Attack ×", 0.1, 10.0, 1.0).log(),
    ParamInfo::new(id::RELEASE, "Release ×", 0.1, 10.0, 1.0).log(),
];

/// How a region loops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Looping {
    /// Plays to the end, stops early on note-off (after the release).
    #[default]
    None,
    /// Plays to the end whatever the note-off.
    OneShot,
    /// Loops between the points until the voice has faded out.
    Continuous,
    /// Loops while the key is held, then plays on to the end.
    Sustain,
}

/// One region, ready to play (built off the audio thread from an SFZ region).
#[derive(Debug, Clone)]
pub struct SfzRegion {
    /// The sample.
    pub audio: Arc<AudioData>,
    /// Key range (inclusive).
    pub keys: (u8, u8),
    /// Velocity range (inclusive, 1 … 127).
    pub vels: (u8, u8),
    /// Key at which the sample plays at its own pitch.
    pub keycenter: f32,
    /// Cents per key.
    pub keytrack: f32,
    /// Fine tuning in cents.
    pub tune: f32,
    /// Level in dB.
    pub volume_db: f32,
    /// −100 (left) … 100 (right).
    pub pan: f32,
    /// How much velocity changes the level (0 … 100 %).
    pub veltrack: f32,
    /// Envelope attack (s).
    pub attack: f32,
    /// Envelope decay (s; 0 = none, the level stays at the top).
    pub decay: f32,
    /// Envelope sustain level (0 … 1).
    pub sustain: f32,
    /// Envelope release (s).
    pub release: f32,
    /// First frame played.
    pub offset: u64,
    /// Plays on note-off (release samples) instead of note-on.
    pub on_release: bool,
    /// Loop mode.
    pub looping: Looping,
    /// Loop start and end (frames, `end` inclusive).
    pub loop_points: Option<(u64, u64)>,
    /// Round robin: plays on every `seq_length`-th hit …
    pub seq_length: u32,
    /// … at this position (1-based).
    pub seq_position: u32,
    /// Random range: plays when the note's random value is in `lorand ..< hirand`.
    pub rand: (f32, f32),
    /// Group (0 = none).
    pub group: i64,
    /// Group whose notes cut this region off (0 = none).
    pub off_by: i64,
}

impl SfzRegion {
    /// A region with SFZ defaults for `audio` (whole keyboard and velocity range, root 60).
    pub fn new(audio: Arc<AudioData>) -> Self {
        Self {
            audio,
            keys: (0, 127),
            vels: (1, 127),
            keycenter: 60.0,
            keytrack: 100.0,
            tune: 0.0,
            volume_db: 0.0,
            pan: 0.0,
            veltrack: 100.0,
            attack: 0.0,
            decay: 0.0,
            sustain: 1.0,
            release: 0.001,
            offset: 0,
            on_release: false,
            looping: Looping::None,
            loop_points: None,
            seq_length: 1,
            seq_position: 1,
            rand: (0.0, 1.0),
            group: 0,
            off_by: 0,
        }
    }

    fn matches(&self, key: u8, vel: u8, rand: f32) -> bool {
        (self.keys.0..=self.keys.1).contains(&key)
            && (self.vels.0..=self.vels.1).contains(&vel)
            && rand >= self.rand.0
            && (rand < self.rand.1 || self.rand.1 >= 1.0)
    }
}

#[derive(Debug, Clone, Default)]
struct Voice {
    region: usize,
    key: Option<u8>,
    pos: f64,
    ratio: f64,
    env: Adsr,
    left: f32,
    right: f32,
    /// Fade-out step when cut (0 = not cut).
    cut: f32,
    cut_gain: f32,
    age: u64,
    playing: bool,
    pedal_held: bool,
}

/// The sample instrument node (no inputs, stereo out).
#[derive(Debug)]
pub struct SfzPlayer {
    values: [f32; PARAMS.len()],
    regions: Vec<SfzRegion>,
    /// Round-robin hit counter per region.
    counters: Vec<u32>,
    voices: Vec<Voice>,
    /// Velocity of the last note-on per key (for release samples).
    last_vel: [u8; 128],
    rng: u32,
    sample_rate: f32,
    clock: u64,
    controllers: Controllers,
}

impl SfzPlayer {
    /// Player for `regions`.
    pub fn new(regions: Vec<SfzRegion>) -> Self {
        let counters = vec![0; regions.len()];
        Self {
            values: PARAMS.map(|p| p.default),
            regions,
            counters,
            voices: Vec::new(),
            last_vel: [64; 128],
            rng: 0x2545_F491,
            sample_rate: 48_000.0,
            clock: 0,
            controllers: Controllers::default(),
        }
    }

    fn v(&self, id: u64) -> f32 {
        self.values.get(id as usize).copied().unwrap_or(0.0)
    }

    fn random(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x as f32 / u32::MAX as f32
    }

    /// Starts every region that matches (`release` = note-off triggered ones).
    fn trigger(&mut self, key: u8, vel: u8, release: bool) {
        let rand = self.random();
        for r in 0..self.regions.len() {
            let Some(reg) = self.regions.get(r) else {
                continue;
            };
            if reg.on_release != release || !reg.matches(key, vel, rand) {
                continue;
            }
            let Some(count) = self.counters.get_mut(r) else {
                continue;
            };
            let hit = *count % reg.seq_length.max(1);
            *count = count.wrapping_add(1);
            if hit + 1 != reg.seq_position.max(1) {
                continue;
            }
            if reg.group != 0 {
                self.cut_group(reg.group);
            }
            self.start(r, key, vel, release);
        }
    }

    /// Fades out voices whose regions are cut off by `group`.
    fn cut_group(&mut self, group: i64) {
        let step = 1.0 / (CUT_FADE * self.sample_rate);
        for v in self.voices.iter_mut().filter(|v| v.playing && v.cut == 0.0) {
            if self
                .regions
                .get(v.region)
                .is_some_and(|r| r.off_by == group)
            {
                v.cut = step;
            }
        }
    }

    fn start(&mut self, region: usize, key: u8, vel: u8, release: bool) {
        let Some(reg) = self.regions.get(region) else {
            return;
        };
        let sr = self.sample_rate;
        let semis = (f32::from(key) - reg.keycenter) * reg.keytrack / 100.0
            + reg.tune / 100.0
            + self.v(id::TUNE);
        let ratio = f64::from((semis / 12.0).exp2()) * f64::from(reg.audio.sample_rate.max(1))
            / f64::from(sr);
        let velocity = f32::from(vel) / 127.0;
        let vel_gain = velocity.powf(2.0 * reg.veltrack / 100.0);
        let gain = 10.0_f32.powf(reg.volume_db / 20.0) * vel_gain;
        let angle = (reg.pan.clamp(-100.0, 100.0) + 100.0) / 200.0 * std::f32::consts::FRAC_PI_2;
        let (left, right) = (
            angle.cos() * std::f32::consts::SQRT_2 * gain,
            angle.sin() * std::f32::consts::SQRT_2 * gain,
        );
        let (a, r) = (
            reg.attack * self.v(id::ATTACK),
            reg.release * self.v(id::RELEASE),
        );
        let (decay, sustain, offset) = (reg.decay, reg.sustain, reg.offset);
        self.clock += 1;
        let clock = self.clock;
        let idx = self.voices.iter().position(|v| !v.playing).or_else(|| {
            self.voices
                .iter()
                .enumerate()
                .min_by_key(|(_, v)| v.age)
                .map(|(i, _)| i)
        });
        let Some(v) = idx.and_then(|i| self.voices.get_mut(i)) else {
            return;
        };
        *v = Voice {
            region,
            // Release samples are not tied to a held key.
            key: (!release).then_some(key),
            pos: offset as f64,
            ratio,
            env: Adsr::default(),
            left,
            right,
            cut: 0.0,
            cut_gain: 1.0,
            age: clock,
            playing: true,
            pedal_held: false,
        };
        // No decay time: hold the attack level (sustain = 1).
        let (decay, sustain) = if decay <= 0.0 {
            (0.01, 1.0)
        } else {
            (decay, sustain)
        };
        v.env.set(a, decay, sustain, r.max(0.001), sr);
        v.env.gate_on();
    }

    fn release_voice(v: &mut Voice, looping: Looping) {
        v.key = None;
        if looping != Looping::OneShot {
            v.env.gate_off();
        }
    }

    fn note_off(&mut self, key: u8) {
        let sustain = self.controllers.sustain;
        for i in 0..self.voices.len() {
            let Some(v) = self.voices.get_mut(i) else {
                continue;
            };
            if v.key != Some(key) || v.pedal_held {
                continue;
            }
            if sustain {
                v.pedal_held = true;
            } else {
                let looping = self
                    .regions
                    .get(v.region)
                    .map_or(Looping::None, |r| r.looping);
                Self::release_voice(v, looping);
            }
        }
        let vel = self.last_vel.get(key as usize).copied().unwrap_or(64);
        self.trigger(key, vel, true);
    }

    fn controller(&mut self, number: u8, val: f32) {
        if self.controllers.set(number, val) == Change::PedalUp {
            for v in self.voices.iter_mut().filter(|v| v.pedal_held) {
                v.pedal_held = false;
                let looping = self
                    .regions
                    .get(v.region)
                    .map_or(Looping::None, |r| r.looping);
                Self::release_voice(v, looping);
            }
        }
    }

    fn render(&mut self, io: &mut AudioIo, from: usize, to: usize) {
        let vol = self.v(id::VOLUME) * self.controllers.gain();
        for i in from..to {
            let mut out = [0.0_f32; 2];
            for v in &mut self.voices {
                if !v.playing {
                    continue;
                }
                let Some(reg) = self.regions.get(v.region) else {
                    v.playing = false;
                    continue;
                };
                let len = reg.audio.frames();
                let e = v.env.next_value();
                if v.cut > 0.0 {
                    v.cut_gain -= v.cut;
                }
                if !v.env.is_active() || v.cut_gain <= 0.0 || v.pos >= len as f64 {
                    v.playing = false;
                    v.key = None;
                    continue;
                }
                let chans = &reg.audio.channels;
                let l = chans.first().map_or(0.0, |c| cubic(c, v.pos));
                let r = chans.get(1).map_or(l, |c| cubic(c, v.pos));
                let g = e * v.cut_gain;
                out[0] += l * v.left * g;
                out[1] += r * v.right * g;
                v.pos += v.ratio;
                let loops = match reg.looping {
                    Looping::Continuous => true,
                    Looping::Sustain => v.key.is_some() || v.pedal_held,
                    Looping::None | Looping::OneShot => false,
                };
                if let (true, Some((start, end))) = (loops, reg.loop_points) {
                    let (start, end) = (start as f64, (end + 1) as f64);
                    if end - start >= 2.0 && v.pos >= end {
                        v.pos -= end - start;
                    }
                }
            }
            for (ch, y) in out.iter().enumerate() {
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = y * vol;
                }
            }
        }
    }
}

impl AudioNode for SfzPlayer {
    fn num_inputs(&self) -> usize {
        0
    }

    fn num_outputs(&self) -> usize {
        2
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        self.voices = vec![Voice::default(); VOICES];
    }

    fn process(&mut self, _ctx: &ProcessContext, io: &mut AudioIo, events: &[Event]) {
        let n = io.frames();
        let mut pos = 0;
        for e in events {
            let at = (e.offset as usize).min(n);
            self.render(io, pos, at);
            pos = at;
            match e.kind {
                EventKind::NoteOn { key, vel, .. } => {
                    let vel = (vel * 127.0).round().clamp(1.0, 127.0) as u8;
                    if let Some(slot) = self.last_vel.get_mut(key as usize) {
                        *slot = vel;
                    }
                    self.trigger(key, vel, false);
                }
                EventKind::NoteOff { key, .. } => self.note_off(key),
                EventKind::AllNotesOff => {
                    self.controllers.sustain = false;
                    for v in &mut self.voices {
                        v.key = None;
                        v.pedal_held = false;
                        v.env.gate_off();
                    }
                }
                EventKind::Param { id, val } => self.set_param(id, val),
                EventKind::Cc { cc, val, .. } => self.controller(cc, val),
            }
        }
        self.render(io, pos, n);
    }

    fn reset(&mut self) {
        for v in &mut self.voices {
            *v = Voice::default();
        }
        for c in &mut self.counters {
            *c = 0;
        }
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use musiton_core::{TempoMap, TimeSig};

    /// Windowed single-frequency DFT magnitude.
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

    fn sine(hz: f32, secs: f32) -> Arc<AudioData> {
        let n = (secs * 48_000.0) as usize;
        Arc::new(AudioData {
            sample_rate: 48_000,
            channels: vec![(0..n)
                .map(|i| (std::f32::consts::TAU * hz * i as f32 / 48_000.0).sin() * 0.5)
                .collect()],
        })
    }

    fn ev(offset: u32, key: u8, on: bool, vel: f32) -> Event {
        let kind = if on {
            EventKind::NoteOn { ch: 0, key, vel }
        } else {
            EventKind::NoteOff { ch: 0, key }
        };
        Event { offset, kind }
    }

    fn render(p: &mut SfzPlayer, events: &[Event], frames: usize) -> Vec<f32> {
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
        p.process(&ctx, &mut AudioIo::new(&[], &mut outs, frames), events);
        outs.swap_remove(0)
    }

    fn player(regions: Vec<SfzRegion>) -> SfzPlayer {
        let mut p = SfzPlayer::new(regions);
        p.prepare(48_000.0, 48_000);
        p
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    #[test]
    fn key_and_velocity_pick_the_region_and_pitch_follows() {
        let soft = SfzRegion {
            keys: (60, 72),
            vels: (1, 64),
            ..SfzRegion::new(sine(440.0, 1.0))
        };
        let loud = SfzRegion {
            vels: (65, 127),
            ..SfzRegion {
                keys: (60, 72),
                ..SfzRegion::new(sine(1000.0, 1.0))
            }
        };
        let quiet = render(
            &mut player(vec![soft.clone(), loud.clone()]),
            &[ev(0, 60, true, 0.3)],
            9_600,
        );
        assert!(magnitude(&quiet[960..], 440.0) > 10.0 * magnitude(&quiet[960..], 1000.0));
        let hard = render(
            &mut player(vec![soft.clone(), loud]),
            &[ev(0, 60, true, 0.9)],
            9_600,
        );
        assert!(magnitude(&hard[960..], 1000.0) > 10.0 * magnitude(&hard[960..], 440.0));
        // An octave up from the key center plays at double speed.
        let up = render(
            &mut player(vec![soft.clone()]),
            &[ev(0, 72, true, 0.3)],
            9_600,
        );
        assert!(magnitude(&up[960..], 880.0) > 10.0 * magnitude(&up[960..], 440.0));
        // Outside the key range: silence.
        let none = render(&mut player(vec![soft]), &[ev(0, 50, true, 0.3)], 9_600);
        assert!(rms(&none) < 1e-6);
    }

    #[test]
    fn round_robin_alternates() {
        let a = SfzRegion {
            seq_length: 2,
            seq_position: 1,
            ..SfzRegion::new(sine(440.0, 1.0))
        };
        let b = SfzRegion {
            seq_length: 2,
            seq_position: 2,
            ..SfzRegion::new(sine(1000.0, 1.0))
        };
        let mut p = player(vec![a, b]);
        let first = render(
            &mut p,
            &[ev(0, 60, true, 0.8), ev(100, 60, false, 0.0)],
            4_800,
        );
        let second = render(&mut p, &[ev(0, 60, true, 0.8)], 9_600);
        let third = render(&mut p, &[ev(0, 61, true, 0.8)], 9_600);
        let (x1, x2) = (&first[..100], &second[2_400..]);
        assert!(rms(x1) > 0.05);
        assert!(magnitude(x2, 1000.0) > 10.0 * magnitude(x2, 440.0));
        // The third hit is the first variation again (mixed with the still sounding second).
        assert!(magnitude(&third[..4_800], 440.0 * 2f64.powf(1.0 / 12.0)) > 1.0);
    }

    #[test]
    fn release_samples_play_on_note_off() {
        let body = SfzRegion::new(sine(440.0, 1.0));
        let tail = SfzRegion {
            on_release: true,
            ..SfzRegion::new(sine(1000.0, 0.5))
        };
        let out = render(
            &mut player(vec![body, tail]),
            &[ev(0, 60, true, 0.8), ev(4_800, 60, false, 0.0)],
            19_200,
        );
        assert!(magnitude(&out[..4_800], 1000.0) < 0.1 * magnitude(&out[..4_800], 440.0));
        assert!(magnitude(&out[7_200..], 1000.0) > 10.0 * magnitude(&out[7_200..], 440.0));
    }

    #[test]
    fn a_group_cuts_another_off() {
        let open = SfzRegion {
            keys: (46, 46),
            keycenter: 46.0,
            group: 2,
            off_by: 1,
            ..SfzRegion::new(sine(440.0, 1.0))
        };
        let closed = SfzRegion {
            keys: (42, 42),
            keycenter: 42.0,
            group: 1,
            ..SfzRegion::new(sine(1000.0, 0.05))
        };
        let out = render(
            &mut player(vec![open, closed]),
            &[ev(0, 46, true, 0.8), ev(4_800, 42, true, 0.8)],
            19_200,
        );
        assert!(rms(&out[..4_800]) > 0.1);
        assert!(rms(&out[9_600..]) < 1e-4, "open hat still sounds");
    }

    #[test]
    fn continuous_loop_sustains_past_the_sample_end() {
        let reg = SfzRegion {
            looping: Looping::Continuous,
            loop_points: Some((0, 4_799)),
            ..SfzRegion::new(sine(100.0, 0.1))
        };
        let out = render(&mut player(vec![reg]), &[ev(0, 60, true, 0.8)], 48_000);
        assert!(rms(&out[40_000..]) > 0.1);
    }

    #[test]
    fn velocity_and_volume_scale_the_level() {
        let reg = SfzRegion::new(sine(440.0, 1.0));
        let soft = rms(&render(
            &mut player(vec![reg.clone()]),
            &[ev(0, 60, true, 0.25)],
            9_600,
        )[960..]);
        let hard = rms(&render(
            &mut player(vec![reg.clone()]),
            &[ev(0, 60, true, 1.0)],
            9_600,
        )[960..]);
        assert!(hard > 4.0 * soft, "{hard} vs {soft}");
        let quieter = SfzRegion {
            volume_db: -6.0,
            ..reg
        };
        let q = rms(&render(&mut player(vec![quieter]), &[ev(0, 60, true, 1.0)], 9_600)[960..]);
        assert!((q / hard - 0.501).abs() < 0.02, "{}", q / hard);
    }
}
