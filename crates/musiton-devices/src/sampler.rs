//! Sampler: plays a sample across the keyboard (root key, ADSR, loop, velocity), or in
//! drum mode one sample per key (pads).

use std::sync::Arc;

use musiton_core::audio::AudioData;
use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_dsp::env::Adsr;
use musiton_dsp::resample::Mipmap;
use musiton_dsp::smooth::LinearRamp;
use musiton_engine::node::{AudioIo, AudioNode, Event, EventKind, ProcessContext};

/// Number of voices.
pub const VOICES: usize = 16;
/// Shortest loop in frames.
const MIN_LOOP: f64 = 32.0;

/// Parameter ids.
#[allow(missing_docs)]
pub mod id {
    pub const ROOT: u64 = 0;
    pub const TUNE: u64 = 1;
    pub const ATTACK: u64 = 2;
    pub const DECAY: u64 = 3;
    pub const SUSTAIN: u64 = 4;
    pub const RELEASE: u64 = 5;
    pub const LOOP: u64 = 6;
    pub const LOOP_START: u64 = 7;
    pub const LOOP_END: u64 = 8;
    pub const VELOCITY: u64 = 9;
    pub const VOLUME: u64 = 10;
    pub const DRUM: u64 = 11;
}

/// All parameters (index = id).
pub const PARAMS: [ParamInfo; 12] = [
    ParamInfo::new(id::ROOT, "Root key", 0.0, 127.0, 60.0).int(),
    ParamInfo::new(id::TUNE, "Tune", -24.0, 24.0, 0.0).unit("st"),
    ParamInfo::new(id::ATTACK, "Attack", 0.001, 5.0, 0.002)
        .log()
        .unit("s"),
    ParamInfo::new(id::DECAY, "Decay", 0.001, 5.0, 0.3)
        .log()
        .unit("s"),
    ParamInfo::new(id::SUSTAIN, "Sustain", 0.0, 1.0, 1.0),
    ParamInfo::new(id::RELEASE, "Release", 0.001, 10.0, 0.2)
        .log()
        .unit("s"),
    ParamInfo::toggle(id::LOOP, "Loop", false),
    ParamInfo::new(id::LOOP_START, "Loop start", 0.0, 1.0, 0.0),
    ParamInfo::new(id::LOOP_END, "Loop end", 0.0, 1.0, 1.0),
    ParamInfo::new(id::VELOCITY, "Velocity", 0.0, 1.0, 0.8),
    ParamInfo::new(id::VOLUME, "Volume", 0.0, 1.0, 0.8),
    ParamInfo::toggle(id::DRUM, "Drum mode", false),
];

/// A sample prepared for playback (built off the audio thread).
#[derive(Debug, Clone)]
pub struct Zone {
    /// Root key (melodic mode) or pad key (drum mode).
    pub key: u8,
    channels: Vec<Mipmap>,
    rate: f64,
    len: usize,
}

impl Zone {
    /// Prepares `audio` for the key `key`.
    pub fn new(key: u8, audio: &Arc<AudioData>) -> Self {
        Self {
            key,
            channels: audio
                .channels
                .iter()
                .take(2)
                .map(|c| Mipmap::new(c))
                .collect(),
            rate: f64::from(audio.sample_rate.max(1)),
            len: audio.frames(),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Voice {
    key: Option<u8>,
    zone: usize,
    pos: f64,
    ratio: f64,
    env: Adsr,
    gain: f32,
    age: u64,
    one_shot: bool,
    playing: bool,
}

/// The sampler node (no inputs, stereo out).
#[derive(Debug)]
pub struct Sampler {
    values: [f32; PARAMS.len()],
    zones: Vec<Zone>,
    voices: Vec<Voice>,
    sample_rate: f32,
    clock: u64,
    volume: LinearRamp,
    dirty: bool,
}

impl Sampler {
    /// Sampler with the given zones (first zone = the melodic sample).
    pub fn new(zones: Vec<Zone>) -> Self {
        Self {
            values: PARAMS.map(|p| p.default),
            zones,
            voices: Vec::new(),
            sample_rate: 48_000.0,
            clock: 0,
            volume: LinearRamp::new(0.8, 1),
            dirty: true,
        }
    }

    fn v(&self, id: u64) -> f32 {
        self.values.get(id as usize).copied().unwrap_or(0.0)
    }

    fn drum(&self) -> bool {
        self.v(id::DRUM) > 0.5
    }

    fn note_on(&mut self, key: u8, vel: f32) {
        let drum = self.drum();
        let zone = if drum {
            self.zones.iter().position(|z| z.key == key)
        } else {
            (!self.zones.is_empty()).then_some(0)
        };
        let Some(zone_idx) = zone else {
            return;
        };
        let Some(z) = self.zones.get(zone_idx) else {
            return;
        };
        let semis = if drum {
            f64::from(self.v(id::TUNE))
        } else {
            f64::from(key) - f64::from(self.v(id::ROOT).round()) + f64::from(self.v(id::TUNE))
        };
        let ratio = (semis / 12.0).exp2() * z.rate / f64::from(self.sample_rate);
        self.clock += 1;
        let idx = self.voices.iter().position(|v| !v.playing).or_else(|| {
            self.voices
                .iter()
                .enumerate()
                .min_by_key(|(_, v)| v.age)
                .map(|(i, _)| i)
        });
        let sens = self.v(id::VELOCITY);
        let clock = self.clock;
        let Some(v) = idx.and_then(|i| self.voices.get_mut(i)) else {
            return;
        };
        v.key = Some(key);
        v.zone = zone_idx;
        // A stolen voice keeps its envelope level (no jump) but restarts the sample.
        v.pos = 0.0;
        v.ratio = ratio;
        v.gain = 1.0 - sens + sens * vel;
        v.age = clock;
        v.one_shot = drum;
        v.playing = true;
        v.env.gate_on();
    }

    fn note_off(&mut self, key: u8) {
        for v in self
            .voices
            .iter_mut()
            .filter(|v| v.key == Some(key) && !v.one_shot)
        {
            v.key = None;
            v.env.gate_off();
        }
    }

    fn render(&mut self, io: &mut AudioIo, from: usize, to: usize) {
        if self.dirty {
            let sr = self.sample_rate;
            let (a, d, s, r) = (
                self.v(id::ATTACK),
                self.v(id::DECAY),
                self.v(id::SUSTAIN),
                self.v(id::RELEASE),
            );
            let drum = self.drum();
            for v in &mut self.voices {
                // Drum pads play the whole sample at full level.
                if drum {
                    v.env.set(0.0005, 5.0, 1.0, r, sr);
                } else {
                    v.env.set(a, d, s, r, sr);
                }
            }
            self.dirty = false;
        }
        let looping = self.v(id::LOOP) > 0.5 && !self.drum();
        let (ls, le) = (
            f64::from(self.v(id::LOOP_START)),
            f64::from(self.v(id::LOOP_END)),
        );
        self.volume.set_target(self.v(id::VOLUME));
        for i in from..to {
            let vol = self.volume.next_value();
            let mut out = [0.0_f32; 2];
            for v in &mut self.voices {
                if !v.playing {
                    continue;
                }
                let Some(z) = self.zones.get(v.zone) else {
                    v.playing = false;
                    continue;
                };
                let e = v.env.next_value();
                if !v.env.is_active() || v.pos >= z.len as f64 {
                    v.playing = false;
                    v.key = None;
                    continue;
                }
                let left = z.channels.first();
                let right = z.channels.get(1).or(left);
                for (ch, src) in [left, right].into_iter().enumerate() {
                    if let Some(m) = src {
                        out[ch] += m.read(v.pos, v.ratio) * e * v.gain;
                    }
                }
                v.pos += v.ratio;
                if looping && v.key.is_some() {
                    let len = z.len as f64;
                    let (start, end) = (ls.min(le) * len, ls.max(le) * len);
                    if end - start >= MIN_LOOP && v.pos >= end {
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

impl AudioNode for Sampler {
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
                    for v in &mut self.voices {
                        v.key = None;
                        v.env.gate_off();
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

#[cfg(test)]
mod tests {
    use super::*;
    use musiton_core::{TempoMap, TimeSig};

    fn sine(hz: f32, secs: f32) -> Arc<AudioData> {
        let n = (48_000.0 * secs) as usize;
        Arc::new(AudioData {
            sample_rate: 48_000,
            channels: vec![(0..n)
                .map(|i| (std::f32::consts::TAU * hz * i as f32 / 48_000.0).sin() * 0.5)
                .collect()],
        })
    }

    fn render(s: &mut Sampler, events: &[Event], frames: usize) -> Vec<f32> {
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

    fn sampler(zones: Vec<Zone>, settings: &[(u64, f32)]) -> Sampler {
        let mut s = Sampler::new(zones);
        for &(i, v) in settings {
            s.set_param(ParamId(i), v);
        }
        s.prepare(48_000.0, 96_000);
        s
    }

    fn ev(offset: u32, key: u8, on: bool, vel: f32) -> Event {
        let kind = if on {
            EventKind::NoteOn { ch: 0, key, vel }
        } else {
            EventKind::NoteOff { ch: 0, key }
        };
        Event { offset, kind }
    }

    fn crossings(x: &[f32]) -> usize {
        x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count()
    }

    #[test]
    fn pitch_follows_the_keyboard() {
        let z = Zone::new(60, &sine(440.0, 2.0));
        let mut s = sampler(vec![z.clone()], &[(id::VELOCITY, 0.0)]);
        let root = render(&mut s, &[ev(0, 60, true, 1.0)], 48_000);
        let mut s = sampler(vec![z], &[(id::VELOCITY, 0.0)]);
        let up = render(&mut s, &[ev(0, 72, true, 1.0)], 24_000);
        assert!((439..=441).contains(&crossings(&root)));
        assert!(
            (439..=441).contains(&crossings(&up)),
            "octave up: {}",
            crossings(&up)
        );
    }

    #[test]
    fn velocity_scales_volume() {
        let z = Zone::new(60, &sine(440.0, 1.0));
        let peak = |vel: f32| {
            let mut s = sampler(vec![z.clone()], &[(id::VELOCITY, 1.0)]);
            render(&mut s, &[ev(0, 60, true, vel)], 4_800)[1_000..]
                .iter()
                .fold(0.0_f32, |m, x| m.max(x.abs()))
        };
        assert!((peak(0.5) / peak(1.0) - 0.5).abs() < 0.02);
    }

    #[test]
    fn loop_sustains_past_the_sample_end() {
        let z = Zone::new(60, &sine(480.0, 0.5));
        let mut s = sampler(
            vec![z],
            &[
                (id::LOOP, 1.0),
                (id::LOOP_START, 0.25),
                (id::LOOP_END, 0.75),
            ],
        );
        let out = render(&mut s, &[ev(0, 60, true, 1.0)], 96_000);
        assert!(
            out[90_000..].iter().any(|x| x.abs() > 0.1),
            "still sounding after 1.9 s"
        );
        let mut s = sampler(vec![Zone::new(60, &sine(480.0, 0.5))], &[]);
        let out = render(&mut s, &[ev(0, 60, true, 1.0)], 48_000);
        assert!(
            out[25_000..].iter().all(|x| *x == 0.0),
            "no loop: stops at the end"
        );
    }

    #[test]
    fn drum_mode_plays_one_sample_per_key_ignoring_note_off() {
        let kick = Zone::new(36, &sine(100.0, 0.5));
        let hat = Zone::new(42, &sine(4_000.0, 0.1));
        let mut s = sampler(vec![kick, hat], &[(id::DRUM, 1.0)]);
        let out = render(
            &mut s,
            &[
                ev(0, 42, true, 1.0),
                ev(10, 42, false, 0.0),
                ev(9_600, 40, true, 1.0),
            ],
            9_600 + 2_000,
        );
        assert!((390..=410).contains(&crossings(&out[..4_800])), "hat pitch");
        assert!(
            out[9_600..].iter().all(|x| *x == 0.0),
            "key without pad is silent"
        );
    }

    #[test]
    fn high_notes_do_not_alias() {
        let z = Zone::new(60, &sine(8_000.0, 1.0));
        let mut s = sampler(vec![z], &[]);
        // +2 octaves → 32 kHz, which would fold to 16 kHz without the octave filters.
        let out = render(&mut s, &[ev(0, 84, true, 1.0)], 9_600);
        let rms = (out[2_000..].iter().map(|x| x * x).sum::<f32>() / 7_600.0).sqrt();
        assert!(rms < 0.005, "aliased energy {rms}");
    }
}
