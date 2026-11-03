//! A small sine synth used until the full polysynth exists (phase 9).

use musiton_dsp::smooth::LinearRamp;
use musiton_engine::node::{AudioIo, AudioNode, Event, EventKind, ProcessContext};

/// Number of voices.
pub const VOICES: usize = 16;
const ATTACK_MS: f32 = 5.0;
const RELEASE_MS: f32 = 150.0;
const LEVEL: f32 = 0.2;

#[derive(Debug, Clone)]
struct Voice {
    key: Option<u8>,
    phase: f32,
    inc: f32,
    amp: LinearRamp,
    vel: f32,
    age: u64,
}

/// Polyphonic sine synth (stereo out). Oldest voice is stolen when all are busy.
#[derive(Debug)]
pub struct SimpleSynth {
    voices: Vec<Voice>,
    sample_rate: f32,
    clock: u64,
}

impl Default for SimpleSynth {
    fn default() -> Self {
        Self::new()
    }
}

impl SimpleSynth {
    /// New synth (call `prepare` before use).
    pub fn new() -> Self {
        Self {
            voices: Vec::new(),
            sample_rate: 48_000.0,
            clock: 0,
        }
    }

    fn note_on(&mut self, key: u8, vel: f32) {
        self.clock += 1;
        let free = self
            .voices
            .iter()
            .position(|v| v.key.is_none() && !v.amp.is_ramping() && v.amp.target() == 0.0);
        let idx = free.unwrap_or_else(|| {
            self.voices
                .iter()
                .enumerate()
                .min_by_key(|(_, v)| v.age)
                .map_or(0, |(i, _)| i)
        });
        let sr = self.sample_rate;
        let clock = self.clock;
        if let Some(v) = self.voices.get_mut(idx) {
            let hz = 440.0 * 2f32.powf((f32::from(key) - 69.0) / 12.0);
            v.key = Some(key);
            v.inc = hz / sr;
            v.vel = vel;
            v.age = clock;
            v.amp = LinearRamp::with_time(v.amp.next_value(), ATTACK_MS, sr);
            v.amp.set_target(1.0);
        }
    }

    fn note_off(&mut self, key: u8) {
        let sr = self.sample_rate;
        for v in self.voices.iter_mut().filter(|v| v.key == Some(key)) {
            v.key = None;
            v.amp = LinearRamp::with_time(v.amp.next_value(), RELEASE_MS, sr);
            v.amp.set_target(0.0);
        }
    }

    fn handle(&mut self, kind: EventKind) {
        match kind {
            EventKind::NoteOn { key, vel, .. } => self.note_on(key, vel),
            EventKind::NoteOff { key, .. } => self.note_off(key),
            EventKind::AllNotesOff => {
                for k in 0..128 {
                    self.note_off(k);
                }
            }
            _ => {}
        }
    }

    fn render(&mut self, io: &mut AudioIo, from: usize, to: usize) {
        for i in from..to {
            let mut s = 0.0;
            for v in &mut self.voices {
                let a = v.amp.next_value();
                if a > 0.0 {
                    s += (v.phase * std::f32::consts::TAU).sin() * a * v.vel * LEVEL;
                    v.phase = (v.phase + v.inc).fract();
                }
            }
            for ch in 0..2 {
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = s;
                }
            }
        }
    }
}

impl AudioNode for SimpleSynth {
    fn num_inputs(&self) -> usize {
        0
    }

    fn num_outputs(&self) -> usize {
        2
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        self.voices = (0..VOICES)
            .map(|_| Voice {
                key: None,
                phase: 0.0,
                inc: 0.0,
                amp: LinearRamp::new(0.0, 1),
                vel: 0.0,
                age: 0,
            })
            .collect();
    }

    fn process(&mut self, _ctx: &ProcessContext, io: &mut AudioIo, events: &[Event]) {
        let n = io.frames();
        let mut pos = 0;
        for e in events {
            let at = (e.offset as usize).min(n);
            self.render(io, pos, at);
            pos = at;
            self.handle(e.kind);
        }
        self.render(io, pos, n);
    }

    fn reset(&mut self) {
        for v in &mut self.voices {
            v.key = None;
            v.amp.set_immediate(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use musiton_core::{TempoMap, TimeSig};

    fn render(synth: &mut SimpleSynth, events: &[Event], frames: usize) -> Vec<f32> {
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
        synth.process(&ctx, &mut AudioIo::new(&[], &mut outs, frames), events);
        outs.swap_remove(0)
    }

    #[test]
    fn note_starts_on_its_sample_and_releases_without_clicks() {
        let mut s = SimpleSynth::new();
        s.prepare(48_000.0, 48_000);
        let on = Event {
            offset: 1000,
            kind: EventKind::NoteOn {
                ch: 0,
                key: 69,
                vel: 1.0,
            },
        };
        let off = Event {
            offset: 20_000,
            kind: EventKind::NoteOff { ch: 0, key: 69 },
        };
        let out = render(&mut s, &[on, off], 48_000);
        assert!(out[..1001].iter().all(|&x| x == 0.0));
        assert!(out[1002] != 0.0);
        let peak = out[5_000..19_000]
            .iter()
            .fold(0.0_f32, |m, x| m.max(x.abs()));
        assert!((peak - LEVEL).abs() < 1e-3, "peak {peak}");
        let crossings = out[5_000..19_000]
            .windows(2)
            .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
            .count();
        assert!((128..=129).contains(&crossings), "≈440 Hz: {crossings}");
        assert!(
            out[20_000 + 7_300..].iter().all(|&x| x == 0.0),
            "released after 150 ms"
        );
        let worst = out
            .windows(2)
            .fold(0.0_f32, |m, w| m.max((w[1] - w[0]).abs()));
        assert!(
            worst < LEVEL * std::f32::consts::TAU * 440.0 / 48_000.0 * 1.2,
            "click: {worst}"
        );
    }

    #[test]
    fn more_notes_than_voices_steal_the_oldest() {
        let mut s = SimpleSynth::new();
        s.prepare(48_000.0, 4_800);
        let events: Vec<Event> = (0..20)
            .map(|k| Event {
                offset: k * 10,
                kind: EventKind::NoteOn {
                    ch: 0,
                    key: 40 + k as u8,
                    vel: 0.5,
                },
            })
            .collect();
        render(&mut s, &events, 4_800);
        let keys: Vec<u8> = s.voices.iter().filter_map(|v| v.key).collect();
        assert_eq!(keys.len(), VOICES);
        assert!(!keys.contains(&40) && keys.contains(&59));
    }
}
