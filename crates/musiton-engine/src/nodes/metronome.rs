//! Metronome click (accent on beat 1 of each bar).

use crate::node::{AudioIo, AudioNode, Event, ProcessContext};

/// Accented click frequency (beat 1).
const ACCENT_HZ: f32 = 1760.0;
/// Normal click frequency.
const BEAT_HZ: f32 = 880.0;
/// Accented click level.
const ACCENT_LEVEL: f32 = 0.5;
/// Normal click level.
const BEAT_LEVEL: f32 = 0.35;
/// Decay time constant of the click envelope (seconds).
const DECAY_TAU: f32 = 0.008;
/// Click length (seconds); after this the voice stops.
const CLICK_LEN: f32 = 0.05;

/// Plays a short sine "blip" on every beat while the transport runs and the metronome is on.
/// One mono output.
#[derive(Debug, Default)]
pub struct Metronome {
    sample_rate: f32,
    decay: f32,
    len: u32,
    phase: f32,
    inc: f32,
    env: f32,
    remaining: u32,
}

impl Metronome {
    /// New metronome (call `prepare` before use).
    pub fn new() -> Self {
        Self::default()
    }

    fn trigger(&mut self, accent: bool) {
        let (hz, level) = if accent {
            (ACCENT_HZ, ACCENT_LEVEL)
        } else {
            (BEAT_HZ, BEAT_LEVEL)
        };
        self.phase = 0.0;
        self.inc = hz / self.sample_rate.max(1.0);
        self.env = level;
        self.remaining = self.len;
    }

    #[inline]
    fn next_sample(&mut self) -> f32 {
        if self.remaining == 0 {
            return 0.0;
        }
        self.remaining -= 1;
        let s = (self.phase * std::f32::consts::TAU).sin() * self.env;
        self.phase = (self.phase + self.inc).fract();
        self.env *= self.decay;
        s
    }
}

impl AudioNode for Metronome {
    fn num_inputs(&self) -> usize {
        0
    }

    fn num_outputs(&self) -> usize {
        1
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        self.decay = (-1.0 / (DECAY_TAU * sample_rate.max(1.0))).exp();
        self.len = (CLICK_LEN * sample_rate) as u32;
        self.remaining = 0;
    }

    fn process(&mut self, ctx: &ProcessContext, io: &mut AudioIo, _events: &[Event]) {
        let sr = f64::from(ctx.sample_rate);
        let beat_len = ctx.time_sig.ticks_per_beat();
        let beats_per_bar = i64::from(ctx.time_sig.num);
        let pos = ctx.sample_pos as i64;
        let active = ctx.playing && ctx.metronome && beat_len > 0;
        let beat_sample =
            |k: i64| ctx.tempo.tick_to_sample((k * beat_len) as f64, sr).round() as i64 - pos;

        // First beat whose (rounded) sample position is inside or after this block.
        let mut k = (ctx.tempo.sample_to_tick(pos as f64, sr) / beat_len as f64).floor() as i64 - 1;
        let mut next = beat_sample(k);
        while active && next < 0 {
            k += 1;
            next = beat_sample(k);
        }

        let out = io.output(0);
        for (i, s) in out.iter_mut().enumerate() {
            while active && next == i as i64 {
                self.trigger(k.rem_euclid(beats_per_bar) == 0);
                k += 1;
                next = beat_sample(k);
            }
            *s = self.next_sample();
        }
    }

    fn reset(&mut self) {
        self.remaining = 0;
    }
}
