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
/// first two inputs when bypassed. A fully bypassed effect is not processed.
pub struct FxSlot {
    inner: Box<dyn AudioNode>,
    mix: LinearRamp,
    bypass_init: bool,
}

impl FxSlot {
    /// Slot for `inner`, initially bypassed or not.
    pub fn new(inner: Box<dyn AudioNode>, bypassed: bool) -> Self {
        Self {
            inner,
            mix: LinearRamp::new(if bypassed { 1.0 } else { 0.0 }, 1),
            bypass_init: bypassed,
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
    }

    fn process(&mut self, ctx: &ProcessContext, io: &mut AudioIo, events: &[Event]) {
        let fully_bypassed = !self.mix.is_ramping() && self.mix.target() >= 1.0;
        if !fully_bypassed {
            self.inner.process(ctx, io, events);
        }
        if !self.mix.is_ramping() && self.mix.target() == 0.0 {
            return;
        }
        let n = io.frames();
        for i in 0..n {
            let b = self.mix.next_value();
            for ch in 0..2 {
                let dry = io.input(ch).get(i).copied().unwrap_or(0.0);
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = if fully_bypassed {
                        dry
                    } else {
                        *o * (1.0 - b) + dry * b
                    };
                }
            }
        }
    }

    fn reset(&mut self) {
        self.inner.reset();
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
}
