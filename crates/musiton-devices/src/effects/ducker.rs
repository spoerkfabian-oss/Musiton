//! Ducker: tempo-synced volume dips ("sidechain pumping" without a sidechain). On each
//! beat (or 8th, half note, bar) the level drops by `depth`, holds, then comes back over
//! `release`; `pre` fades in before the next beat so the dip starts clean.

use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_core::PPQ;
use musiton_engine::node::{AudioIo, AudioNode, Event, ProcessContext};

use super::{db, store};

/// Parameter ids.
#[allow(missing_docs)]
pub mod id {
    pub const DEPTH: u64 = 0;
    pub const RATE: u64 = 1;
    pub const HOLD: u64 = 2;
    pub const RELEASE: u64 = 3;
    pub const PRE: u64 = 4;
}

const RATES: &[&str] = &["1/4", "1/8", "1/2", "1 bar"];

/// Parameters.
pub const PARAMS: [ParamInfo; 5] = [
    ParamInfo::new(id::DEPTH, "Depth", 0.0, 36.0, 12.0).unit("dB"),
    ParamInfo::choice(id::RATE, "Rate", RATES, 0),
    ParamInfo::new(id::HOLD, "Hold", 0.0, 0.2, 0.03).unit("s"),
    ParamInfo::new(id::RELEASE, "Release", 0.01, 1.0, 0.15).unit("s"),
    ParamInfo::new(id::PRE, "Pre-dip", 0.0, 0.1, 0.0).unit("s"),
];

/// The ducker effect.
#[derive(Debug)]
pub struct Ducker {
    values: [f32; 5],
    sample_rate: f32,
    /// Phase (beats) while the transport is stopped, so it still pumps.
    free_beats: f64,
    /// Smoothed gain (avoids clicks at the dip).
    gain: f32,
}

impl Default for Ducker {
    fn default() -> Self {
        Self {
            values: PARAMS.map(|p| p.default),
            sample_rate: 48_000.0,
            free_beats: 0.0,
            gain: 1.0,
        }
    }
}

impl Ducker {
    fn v(&self, id: u64) -> f32 {
        self.values.get(id as usize).copied().unwrap_or(0.0)
    }

    /// Dip amount 0 … 1 at `t` seconds after the beat, `left` seconds before the next.
    fn shape(hold: f32, release: f32, pre: f32, t: f32, left: f32) -> f32 {
        let after = if t < hold {
            1.0
        } else {
            let x = ((t - hold) / release.max(1e-3)).min(1.0);
            (1.0 - x) * (1.0 - x)
        };
        let before = if pre > 0.0 && left < pre {
            1.0 - left / pre
        } else {
            0.0
        };
        after.max(before)
    }
}

impl AudioNode for Ducker {
    fn num_inputs(&self) -> usize {
        2
    }

    fn num_outputs(&self) -> usize {
        2
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
    }

    fn process(&mut self, ctx: &ProcessContext, io: &mut AudioIo, _events: &[Event]) {
        let n = io.frames();
        let sr = f64::from(self.sample_rate);
        let period = match self.v(id::RATE).round() as i32 {
            1 => 0.5,
            2 => 2.0,
            3 => f64::from(ctx.time_sig.num.max(1)) * 4.0 / f64::from(ctx.time_sig.den.max(1)),
            _ => 1.0,
        };
        // Beats at the block start and per sample (linear over one block).
        let (start, step) = if ctx.playing {
            let a = ctx.tempo.sample_to_tick(ctx.sample_pos as f64, sr) / PPQ as f64;
            let b = ctx
                .tempo
                .sample_to_tick((ctx.sample_pos + n as u64) as f64, sr)
                / PPQ as f64;
            (a, (b - a) / n.max(1) as f64)
        } else {
            let tick = ctx.tempo.sample_to_tick(ctx.sample_pos as f64, sr);
            let step = ctx.tempo.bpm_at(tick) / 60.0 / sr;
            let start = self.free_beats;
            self.free_beats = (self.free_beats + step * n as f64) % 64.0;
            (start, step)
        };
        let secs_per_beat = if step > 0.0 { 1.0 / (step * sr) } else { 0.5 };
        let (depth, hold, release, pre) = (
            self.v(id::DEPTH),
            self.v(id::HOLD),
            self.v(id::RELEASE),
            self.v(id::PRE),
        );
        let smooth = 1.0 - (-1.0 / (0.001 * self.sample_rate)).exp();
        for i in 0..n {
            let beats = start + step * i as f64;
            let ph = (beats / period).rem_euclid(1.0);
            let t = (ph * period * secs_per_beat) as f32;
            let left = ((1.0 - ph) * period * secs_per_beat) as f32;
            let target = db(-depth * Self::shape(hold, release, pre, t, left));
            self.gain += (target - self.gain) * smooth;
            for ch in 0..2 {
                let x = io.input(ch).get(i).copied().unwrap_or(0.0);
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = x * self.gain;
                }
            }
        }
    }

    fn reset(&mut self) {
        self.gain = 1.0;
        self.free_beats = 0.0;
    }

    fn params(&self) -> &[ParamInfo] {
        &PARAMS
    }

    fn set_param(&mut self, id: ParamId, value: f32) {
        store(&mut self.values, &PARAMS, id.0, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use musiton_core::{TempoMap, TimeSig};

    fn run(d: &mut Ducker, playing: bool, frames: usize) -> Vec<f32> {
        let tempo = TempoMap::constant(120.0);
        let ins = [vec![0.5; frames], vec![0.5; frames]];
        let mut outs = vec![vec![0.0; frames]; 2];
        let ctx = ProcessContext {
            sample_rate: 48_000.0,
            frames,
            playing,
            sample_pos: 0,
            tempo: &tempo,
            time_sig: TimeSig::default(),
            metronome: false,
            inputs: &[],
            recording: false,
            punch: None,
            live_midi: &[],
        };
        d.prepare(48_000.0, frames);
        d.process(&ctx, &mut AudioIo::new(&ins, &mut outs, frames), &[]);
        outs.swap_remove(0)
    }

    #[test]
    fn dips_on_every_beat_and_recovers() {
        let mut d = Ducker::default();
        // 120 BPM: a beat every 24 000 samples. Depth 12 dB, hold 30 ms, release 150 ms.
        let x = run(&mut d, true, 48_000);
        let at = |i: usize| 20.0 * (x[i] / 0.5).log10();
        assert!(
            (at(480) + 12.0).abs() < 0.5,
            "dipped on the beat: {}",
            at(480)
        );
        assert!(
            at(20_000).abs() < 0.1,
            "back before the next: {}",
            at(20_000)
        );
        assert!((at(24_480) + 12.0).abs() < 0.5, "next beat: {}", at(24_480));
        assert!(at(3_000) > at(1_800) && at(1_800) < 0.0, "recovering");
        // Stopped, it still pumps at the song tempo.
        let mut d = Ducker::default();
        let y = run(&mut d, false, 48_000);
        assert!(y[24_480] < 0.2 && y[20_000] > 0.49);
    }

    #[test]
    fn pre_dip_fades_down_before_the_beat() {
        let mut d = Ducker::default();
        d.set_param(ParamId(id::PRE), 0.04);
        let x = run(&mut d, true, 48_000);
        // 20 ms before the second beat: halfway into the pre-dip.
        assert!(x[23_040] < 0.45 && x[23_040] > 0.2, "{}", x[23_040]);
    }
}
