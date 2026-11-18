//! Effect slot: runs an effect with a click-free bypass, and a lock-free analyzer tap.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;

use musiton_core::ids::ParamId;
use musiton_dsp::smooth::LinearRamp;

use crate::node::{AudioIo, AudioNode, Event, ParamInfo, ProcessContext};

/// Slot parameter: 1 = bypassed. (Effect parameters use ids below 100.)
pub const FX_BYPASS: ParamId = ParamId(200);
/// Bypass cross-fade time.
const BYPASS_MS: f32 = 10.0;

/// Wraps an effect: inputs/outputs are the effect's; the first two outputs fade to the
/// first two inputs when bypassed. A fully bypassed effect is not processed. The bypassed
/// signal is delayed by the effect's latency, so bypassing keeps the timing that delay
/// compensation counts on.
pub struct FxSlot {
    inner: Box<dyn AudioNode>,
    mix: LinearRamp,
    bypass_init: bool,
    /// The input delayed by the effect's latency (one ring per channel; empty = none).
    dry: [Vec<f32>; 2],
    dry_pos: usize,
}

impl FxSlot {
    /// Slot for `inner`, initially bypassed or not.
    pub fn new(inner: Box<dyn AudioNode>, bypassed: bool) -> Self {
        Self {
            inner,
            mix: LinearRamp::new(if bypassed { 1.0 } else { 0.0 }, 1),
            bypass_init: bypassed,
            dry: [Vec::new(), Vec::new()],
            dry_pos: 0,
        }
    }
}

impl AudioNode for FxSlot {
    fn num_inputs(&self) -> usize {
        self.inner.num_inputs()
    }

    fn num_outputs(&self) -> usize {
        self.inner.num_outputs()
    }

    fn prepare(&mut self, sample_rate: f32, max_block: usize) {
        self.mix = LinearRamp::with_time(
            if self.bypass_init { 1.0 } else { 0.0 },
            BYPASS_MS,
            sample_rate,
        );
        self.inner.prepare(sample_rate, max_block);
        let latency = self.inner.latency() as usize;
        self.dry = [vec![0.0; latency], vec![0.0; latency]];
        self.dry_pos = 0;
    }

    fn process(&mut self, ctx: &ProcessContext, io: &mut AudioIo, events: &[Event]) {
        let fully_bypassed = !self.mix.is_ramping() && self.mix.target() >= 1.0;
        if !fully_bypassed {
            self.inner.process(ctx, io, events);
        }
        let active = self.mix.is_ramping() || self.mix.target() != 0.0;
        let len = self.dry[0].len();
        if !active && len == 0 {
            return;
        }
        let n = io.frames();
        for i in 0..n {
            let b = if active { self.mix.next_value() } else { 0.0 };
            for ch in 0..2 {
                let x = io.input(ch).get(i).copied().unwrap_or(0.0);
                // Keep the delayed copy current even while the effect plays, so a bypass
                // starts from the right samples.
                let dry = match self.dry[ch].get_mut(self.dry_pos) {
                    Some(slot) if len > 0 => std::mem::replace(slot, x),
                    _ => x,
                };
                if !active {
                    continue;
                }
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = if fully_bypassed {
                        dry
                    } else {
                        *o * (1.0 - b) + dry * b
                    };
                }
            }
            if len > 0 {
                self.dry_pos = (self.dry_pos + 1) % len;
            }
        }
    }

    fn reset(&mut self) {
        self.inner.reset();
        for d in &mut self.dry {
            d.fill(0.0);
        }
    }

    fn latency(&self) -> u32 {
        self.inner.latency()
    }

    fn params(&self) -> &[ParamInfo] {
        self.inner.params()
    }

    fn set_param(&mut self, id: ParamId, value: f32) {
        if id == FX_BYPASS {
            self.mix.set_target(if value > 0.5 { 1.0 } else { 0.0 });
        } else {
            self.inner.set_param(id, value);
        }
    }

    fn receive(
        &mut self,
        data: Box<dyn std::any::Any + Send>,
    ) -> Option<Box<dyn std::any::Any + Send>> {
        self.inner.receive(data)
    }
}

/// Size of the analyzer ring (power of two).
pub const ANALYZER_SIZE: usize = 4096;

/// The most recent mono samples of a signal, written by the audio thread and read by the
/// UI for spectrum displays (lock-free; a torn read only affects one frame of the display).
pub struct Analyzer {
    buf: Vec<AtomicU32>,
    write: AtomicUsize,
}

impl Default for Analyzer {
    fn default() -> Self {
        Self {
            buf: (0..ANALYZER_SIZE).map(|_| AtomicU32::new(0)).collect(),
            write: AtomicUsize::new(0),
        }
    }
}

impl Analyzer {
    /// New shared analyzer.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Appends one sample (audio thread).
    #[inline]
    pub fn push(&self, x: f32) {
        let w = self.write.load(Ordering::Relaxed);
        if let Some(slot) = self.buf.get(w % ANALYZER_SIZE) {
            slot.store(x.to_bits(), Ordering::Relaxed);
        }
        self.write.store(w.wrapping_add(1), Ordering::Relaxed);
    }

    /// The last `n` samples, oldest first (UI thread).
    pub fn latest(&self, n: usize) -> Vec<f32> {
        let n = n.min(ANALYZER_SIZE);
        let w = self.write.load(Ordering::Relaxed);
        (0..n)
            .map(|k| {
                let i = w.wrapping_sub(n - k) % ANALYZER_SIZE;
                self.buf
                    .get(i)
                    .map_or(0.0, |s| f32::from_bits(s.load(Ordering::Relaxed)))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyzer_returns_latest_in_order() {
        let a = Analyzer::new();
        for i in 0..5000 {
            a.push(i as f32);
        }
        assert_eq!(a.latest(3), vec![4997.0, 4998.0, 4999.0]);
    }

    /// An impulse through a slot whose effect delays by 10 frames comes out 10 frames
    /// late whether the effect plays or is bypassed.
    #[test]
    fn bypass_keeps_the_effects_latency() {
        use crate::nodes::LatencyDelay;
        use musiton_core::{TempoMap, TimeSig};
        let tempo = TempoMap::default();
        let ctx = ProcessContext {
            sample_rate: 48_000.0,
            frames: 64,
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
        for bypassed in [false, true] {
            let mut slot = FxSlot::new(Box::new(LatencyDelay::new(10)), bypassed);
            slot.prepare(48_000.0, 64);
            let mut input = vec![0.0; 64];
            input[3] = 1.0;
            let ins = vec![input.clone(), input];
            let mut outs = vec![vec![0.0; 64]; 2];
            slot.process(&ctx, &mut AudioIo::new(&ins, &mut outs, 64), &[]);
            let at: Vec<usize> = (0..64).filter(|&i| outs[0][i] != 0.0).collect();
            assert_eq!(at, vec![13], "bypassed = {bypassed}");
        }
    }
}
