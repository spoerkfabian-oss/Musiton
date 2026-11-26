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
pub const VOICES: usize = 128;
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
    /// Plays only while this key switch is the last one pressed.
    pub keyswitch: Option<u8>,
    /// Plays only while each controller is within its range: (cc, lo, hi) on 0 … 127.
    pub cc_ranges: Vec<(u8, f32, f32)>,
    /// Last sample frame played (`None`: the whole sample).
    pub end: Option<u64>,
    /// Seconds the peak is held before the decay.
    pub hold: f32,
    /// Velocity curve points (velocity, gain 0 … 1), sorted; empty: `veltrack` decides.
    pub vel_curve: Vec<(u8, f32)>,
    /// Controller modulation, read when a note starts.
    pub cc_mods: Vec<CcMod>,
    /// Controller crossfades (e.g. between microphone positions).
    pub crossfades: Vec<Crossfade>,
    /// Release samples get this many dB quieter per second the key was held.
    pub rt_decay: f32,
}

/// What a controller modulates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModTarget {
    /// Gain factor: depth % × controller (several multiply).
    Amplitude,
    /// dB added.
    Volume,
    /// Sample frames added to the start offset.
    Offset,
    /// Seconds added to the attack.
    Attack,
    /// Seconds added to the hold.
    Hold,
    /// Seconds added to the decay.
    Decay,
    /// Percent added to the sustain level.
    Sustain,
    /// Seconds added to the release.
    Release,
}

/// `depth` × (controller `cc` / 127) added to (or, for amplitude, multiplied into) `target`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CcMod {
    /// What changes.
    pub target: ModTarget,
    /// Controller number.
    pub cc: u8,
    /// Change at controller value 127.
    pub depth: f32,
}

/// A region fades in (or out) while controller `cc` moves from `lo` to `hi` (power curve).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Crossfade {
    /// Controller number.
    pub cc: u8,
    /// Controller value where the fade starts.
    pub lo: f32,
    /// Controller value where it ends.
    pub hi: f32,
    /// Fades in (`true`) or out.
    pub fade_in: bool,
}

impl Crossfade {
    /// Gain at controller value `v`.
    fn gain(&self, v: f32) -> f32 {
        let t = if self.hi > self.lo {
            ((v - self.lo) / (self.hi - self.lo)).clamp(0.0, 1.0)
        } else if v >= self.hi {
            1.0
        } else {
            0.0
        };
        if self.fade_in {
            t.sqrt()
        } else {
            (1.0 - t).sqrt()
        }
    }
}

/// Gain of `curve` (sorted points, (0, 0) and (127, 1) implied) at velocity `vel`.
fn curve_gain(curve: &[(u8, f32)], vel: u8) -> f32 {
    let mut lo = (0_u8, 0.0_f32);
    for &(v, g) in curve {
        if v >= vel {
            let span = f32::from(v.saturating_sub(lo.0)).max(1.0);
            return lo.1 + (g - lo.1) * f32::from(vel.saturating_sub(lo.0)) / span;
        }
        lo = (v, g);
    }
    let span = f32::from(127_u8.saturating_sub(lo.0)).max(1.0);
    lo.1 + (1.0 - lo.1) * f32::from(vel.saturating_sub(lo.0)) / span
}

/// Instrument-wide settings of an SFZ file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SfzSetup {
    /// Keys that switch articulations instead of playing (inclusive).
    pub keyswitch_range: Option<(u8, u8)>,
    /// Key switch in effect at the start.
    pub keyswitch_default: Option<u8>,
    /// Controller start values (cc, 0 … 127).
    pub cc_defaults: Vec<(u8, f32)>,
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
            keyswitch: None,
            cc_ranges: Vec::new(),
            end: None,
            hold: 0.0,
            vel_curve: Vec::new(),
            cc_mods: Vec::new(),
            crossfades: Vec::new(),
            rt_decay: 0.0,
        }
    }

    fn matches(&self, key: u8, vel: u8, rand: f32, switch: Option<u8>, cc: &[f32; 128]) -> bool {
        (self.keys.0..=self.keys.1).contains(&key)
            && (self.vels.0..=self.vels.1).contains(&vel)
            && rand >= self.rand.0
            && (rand < self.rand.1 || self.rand.1 >= 1.0)
            && self.keyswitch.is_none_or(|k| Some(k) == switch)
            && self
                .cc_ranges
                .iter()
                .all(|&(n, lo, hi)| cc.get(n as usize).is_some_and(|&v| v >= lo && v <= hi))
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
    /// Key switches and controller values (0 … 127).
    setup: SfzSetup,
    switch: Option<u8>,
    cc: [f32; 128],
    rng: u32,
    sample_rate: f32,
    clock: u64,
    controllers: Controllers,
    /// Frame of the event being handled (frames processed since the start).
    now: u64,
    /// Frame of the last note-on per key (for `rt_decay`).
    key_on_at: [u64; 128],
}

impl SfzPlayer {
    /// Player for `regions` with the file's key switch and controller settings.
    pub fn new(regions: Vec<SfzRegion>, setup: SfzSetup) -> Self {
        let counters = vec![0; regions.len()];
        let mut cc = [0.0; 128];
        for &(n, v) in &setup.cc_defaults {
            if let Some(c) = cc.get_mut(n as usize) {
                *c = v;
            }
        }
        Self {
            values: PARAMS.map(|p| p.default),
            regions,
            counters,
            voices: Vec::new(),
            last_vel: [64; 128],
            switch: setup.keyswitch_default,
            cc,
            setup,
            rng: 0x2545_F491,
            sample_rate: 48_000.0,
            clock: 0,
            controllers: Controllers::default(),
            now: 0,
            key_on_at: [0; 128],
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
        if self
            .setup
            .keyswitch_range
            .is_some_and(|(lo, hi)| (lo..=hi).contains(&key))
        {
            if !release {
                self.switch = Some(key);
            }
            return;
        }
        let rand = self.random();
        for r in 0..self.regions.len() {
            let Some(reg) = self.regions.get(r) else {
                continue;
            };
            if reg.on_release != release || !reg.matches(key, vel, rand, self.switch, &self.cc) {
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
        let vel_gain = if reg.vel_curve.is_empty() {
            velocity.powf(2.0 * reg.veltrack / 100.0)
        } else {
            curve_gain(&reg.vel_curve, vel)
        };
        let cc = |n: u8| self.cc.get(n as usize).copied().unwrap_or(0.0) / 127.0;
        let modded = |t: ModTarget| -> f32 {
            reg.cc_mods
                .iter()
                .filter(|m| m.target == t)
                .map(|m| m.depth * cc(m.cc))
                .sum()
        };
        let amplitude: f32 = reg
            .cc_mods
            .iter()
            .filter(|m| m.target == ModTarget::Amplitude)
            .map(|m| m.depth / 100.0 * cc(m.cc))
            .product();
        let fades: f32 = reg
            .crossfades
            .iter()
            .map(|x| x.gain(self.cc.get(x.cc as usize).copied().unwrap_or(0.0)))
            .product();
        let held = if release {
            let on = self
                .key_on_at
                .get(key as usize)
                .copied()
                .unwrap_or(self.now);
            self.now.saturating_sub(on) as f32 / sr.max(1.0)
        } else {
            0.0
        };
        let db = reg.volume_db + modded(ModTarget::Volume) - reg.rt_decay * held;
        let gain = 10.0_f32.powf(db / 20.0) * vel_gain * amplitude * fades;
        // Silent layers (a microphone turned down, the other side of a crossfade) take no voice.
        if gain < 1e-4 {
            return;
        }
        let angle = (reg.pan.clamp(-100.0, 100.0) + 100.0) / 200.0 * std::f32::consts::FRAC_PI_2;
        let (left, right) = (
            angle.cos() * std::f32::consts::SQRT_2 * gain,
            angle.sin() * std::f32::consts::SQRT_2 * gain,
        );
        let (a, r) = (
            (reg.attack + modded(ModTarget::Attack)).max(0.0) * self.v(id::ATTACK),
            (reg.release + modded(ModTarget::Release)).max(0.0) * self.v(id::RELEASE),
        );
        let decay = reg.decay + modded(ModTarget::Decay);
        let sustain = (reg.sustain + modded(ModTarget::Sustain) / 100.0).clamp(0.0, 1.0);
        let hold = reg.hold + modded(ModTarget::Hold);
        let offset = (reg.offset as f32 + modded(ModTarget::Offset)).max(0.0) as u64;
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
        v.env.set_hold(hold, sr);
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
        if let Some(c) = self.cc.get_mut(number as usize) {
            *c = val * 127.0;
        }
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
                let len = reg.end.map_or(reg.audio.frames(), |e| {
                    (e as usize + 1).min(reg.audio.frames())
                });
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
        let block_start = self.now;
        let mut pos = 0;
        for e in events {
            let at = (e.offset as usize).min(n);
            self.render(io, pos, at);
            pos = at;
            self.now = block_start + at as u64;
            match e.kind {
                EventKind::NoteOn { key, vel, .. } => {
                    let vel = (vel * 127.0).round().clamp(1.0, 127.0) as u8;
                    if let Some(slot) = self.last_vel.get_mut(key as usize) {
                        *slot = vel;
                    }
                    if let Some(t) = self.key_on_at.get_mut(key as usize) {
                        *t = self.now;
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
        self.now = block_start + n as u64;
    }

    fn reset(&mut self) {
        for v in &mut self.voices {
            *v = Voice::default();
        }
        for c in &mut self.counters {
            *c = 0;
        }
        self.switch = self.setup.keyswitch_default;
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
        let mut p = SfzPlayer::new(regions, SfzSetup::default());
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
    fn key_switches_and_controllers_choose_regions() {
        let natural = SfzRegion {
            keyswitch: Some(24),
            ..SfzRegion::new(sine(440.0, 1.0))
        };
        let other = SfzRegion {
            keyswitch: Some(25),
            ..SfzRegion::new(sine(1000.0, 1.0))
        };
        let noise = SfzRegion {
            cc_ranges: vec![(20, 1.0, 127.0)],
            ..SfzRegion::new(sine(3000.0, 1.0))
        };
        let setup = SfzSetup {
            keyswitch_range: Some((24, 25)),
            keyswitch_default: Some(24),
            cc_defaults: vec![(20, 0.0)],
        };
        let mut p = SfzPlayer::new(vec![natural, other, noise], setup);
        p.prepare(48_000.0, 48_000);
        let first = render(&mut p, &[ev(0, 60, true, 0.8)], 4_800);
        assert!(magnitude(&first[960..], 440.0) > 10.0 * magnitude(&first[960..], 1000.0));
        assert!(magnitude(&first[960..], 440.0) > 10.0 * magnitude(&first[960..], 3000.0));
        // Pressing the switch key makes no sound; the next note uses the other region.
        p.reset();
        let cc = Event {
            offset: 0,
            kind: EventKind::Cc {
                ch: 0,
                cc: 20,
                val: 1.0,
            },
        };
        let second = render(
            &mut p,
            &[ev(0, 25, true, 0.8), cc, ev(10, 60, true, 0.8)],
            4_800,
        );
        let x = &second[960..];
        assert!(magnitude(x, 1000.0) > 10.0 * magnitude(x, 440.0));
        assert!(
            magnitude(x, 3000.0) > 0.1 * magnitude(x, 1000.0),
            "cc region"
        );
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

    fn cc_event(cc: u8, val: f32) -> Event {
        Event {
            offset: 0,
            kind: EventKind::Cc { ch: 0, cc, val },
        }
    }

    #[test]
    fn controllers_set_microphone_levels_and_crossfades() {
        // Close mic at controller 30 (default full), room mic at 31 (default off).
        let close = SfzRegion {
            cc_mods: vec![CcMod {
                target: ModTarget::Amplitude,
                cc: 30,
                depth: 100.0,
            }],
            ..SfzRegion::new(sine(440.0, 1.0))
        };
        let room = SfzRegion {
            cc_mods: vec![CcMod {
                target: ModTarget::Amplitude,
                cc: 31,
                depth: 100.0,
            }],
            ..SfzRegion::new(sine(1000.0, 1.0))
        };
        // A layer that fades in over controller 1.
        let layer = SfzRegion {
            crossfades: vec![Crossfade {
                cc: 1,
                lo: 0.0,
                hi: 127.0,
                fade_in: true,
            }],
            ..SfzRegion::new(sine(3000.0, 1.0))
        };
        let setup = SfzSetup {
            cc_defaults: vec![(30, 127.0), (31, 0.0), (1, 0.0)],
            ..SfzSetup::default()
        };
        let mut p = SfzPlayer::new(vec![close, room, layer], setup);
        p.prepare(48_000.0, 48_000);
        let x = render(&mut p, &[ev(0, 60, true, 1.0)], 4_800);
        assert!(magnitude(&x[960..], 440.0) > 100.0 * magnitude(&x[960..], 1000.0));
        assert!(magnitude(&x[960..], 440.0) > 100.0 * magnitude(&x[960..], 3000.0));
        assert_eq!(
            p.voices.iter().filter(|v| v.playing).count(),
            1,
            "silent layers take no voice"
        );
        p.reset();
        let y = render(
            &mut p,
            &[cc_event(31, 1.0), cc_event(1, 0.25), ev(0, 60, true, 1.0)],
            4_800,
        );
        let (a, b, c) = (
            magnitude(&y[960..], 440.0),
            magnitude(&y[960..], 1000.0),
            magnitude(&y[960..], 3000.0),
        );
        assert!((b / a - 1.0).abs() < 0.05, "room mic on: {}", b / a);
        // A quarter of the way: power curve → √0.25 = 0.5.
        assert!((c / a - 0.5).abs() < 0.05, "crossfade: {}", c / a);
    }

    #[test]
    fn velocity_curve_hold_end_and_release_decay() {
        // Velocity curve: everything above 64 at full level.
        let flat = SfzRegion {
            vel_curve: vec![(64, 1.0)],
            ..SfzRegion::new(sine(440.0, 1.0))
        };
        let mid = rms(&render(
            &mut player(vec![flat.clone()]),
            &[ev(0, 60, true, 0.5)],
            4_800,
        )[960..]);
        let hard = rms(&render(&mut player(vec![flat]), &[ev(0, 60, true, 1.0)], 4_800)[960..]);
        assert!((mid / hard - 1.0).abs() < 0.03, "{}", mid / hard);
        // Hold 0.1 s, then a fast decay to silence; `end` stops the sample at 0.5 s.
        let held = SfzRegion {
            hold: 0.1,
            decay: 0.02,
            sustain: 0.0,
            ..SfzRegion::new(sine(440.0, 1.0))
        };
        let x = render(&mut player(vec![held]), &[ev(0, 60, true, 1.0)], 9_600);
        assert!(rms(&x[2_400..4_000]) > 0.25, "held");
        assert!(rms(&x[7_200..]) < 0.01, "decayed");
        let short = SfzRegion {
            end: Some(23_999),
            ..SfzRegion::new(sine(440.0, 1.0))
        };
        let y = render(&mut player(vec![short]), &[ev(0, 60, true, 1.0)], 36_000);
        assert!(rms(&y[20_000..23_000]) > 0.25 && rms(&y[25_000..]) == 0.0);
        // A release sample after a held key is quieter by rt_decay dB per second.
        let rel = SfzRegion {
            on_release: true,
            rt_decay: 20.0,
            ..SfzRegion::new(sine(440.0, 1.0))
        };
        let mut p = player(vec![rel]);
        render(&mut p, &[ev(0, 60, true, 1.0)], 24_000);
        let late = render(&mut p, &[ev(0, 60, false, 0.0)], 4_800);
        let mut q = player(vec![SfzRegion {
            on_release: true,
            ..SfzRegion::new(sine(440.0, 1.0))
        }]);
        render(&mut q, &[ev(0, 60, true, 1.0)], 24_000);
        let plain = render(&mut q, &[ev(0, 60, false, 0.0)], 4_800);
        // Held 0.5 s → −10 dB.
        let ratio = rms(&late[960..]) / rms(&plain[960..]);
        assert!((ratio - 0.316).abs() < 0.02, "{ratio}");
    }
}
