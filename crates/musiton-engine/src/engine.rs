//! The audio-thread side of the engine.

use std::sync::Arc;

use musiton_dsp::osc::SineOsc;
use musiton_dsp::smooth::LinearRamp;

use crate::EngineShared;

/// Test tone frequency in Hz.
pub const TEST_TONE_HZ: f64 = 440.0;
/// Test tone peak level (linear, 0.25 ≈ −12 dBFS).
pub const TEST_TONE_LEVEL: f32 = 0.25;
/// Fade time when the test tone is switched on/off.
const TONE_FADE_MS: f32 = 10.0;

/// Renders audio. Lives on the audio thread; everything here is allocation-free.
pub struct Engine {
    shared: Arc<EngineShared>,
    max_block: usize,
    tone: SineOsc,
    tone_gain: LinearRamp,
}

impl Engine {
    /// Creates an engine. `max_block` is the largest number of frames processed at once;
    /// longer buffers are split internally.
    pub fn new(shared: Arc<EngineShared>, sample_rate: f32, max_block: usize) -> Self {
        Self {
            shared,
            max_block: max_block.max(1),
            tone: SineOsc::new(TEST_TONE_HZ, f64::from(sample_rate)),
            tone_gain: LinearRamp::with_time(0.0, TONE_FADE_MS, sample_rate),
        }
    }

    /// Fills `out` (interleaved, `channels` per frame) with the next block of audio.
    /// Any length is accepted; a trailing partial frame is set to silence.
    pub fn process(&mut self, out: &mut [f32], channels: usize) {
        if channels == 0 {
            return;
        }
        let chunk = self.max_block * channels;
        for block in out.chunks_mut(chunk) {
            self.process_block(block, channels);
        }
    }

    fn process_block(&mut self, out: &mut [f32], channels: usize) {
        let target = if self.shared.test_tone() {
            TEST_TONE_LEVEL
        } else {
            0.0
        };
        self.tone_gain.set_target(target);

        if !self.tone_gain.is_ramping() && self.tone_gain.target() == 0.0 {
            self.tone.reset();
            out.fill(0.0);
            return;
        }

        let mut frames = out.chunks_exact_mut(channels);
        for frame in &mut frames {
            let s = self.tone.next_sample() * self.tone_gain.next_value();
            frame.fill(s);
        }
        frames.into_remainder().fill(0.0);
    }
}

/// Renders `frames` frames offline in callbacks of `callback_frames` frames.
/// Used by tests and (later) export; runs exactly the same code as the device callback.
pub fn render_offline(
    engine: &mut Engine,
    frames: usize,
    channels: usize,
    callback_frames: usize,
) -> Vec<f32> {
    let mut out = vec![0.0; frames * channels];
    for buf in out.chunks_mut(callback_frames.max(1) * channels.max(1)) {
        engine.process(buf, channels);
    }
    out
}
