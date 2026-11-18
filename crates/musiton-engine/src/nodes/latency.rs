//! Plugin delay compensation: a fixed stereo delay.

use crate::node::{AudioIo, AudioNode, Event, ProcessContext};

/// Delays its stereo input by a fixed number of frames, so signal paths with less
/// processing latency line up with the slowest one where they meet.
#[derive(Debug)]
pub struct LatencyDelay {
    /// One ring buffer per channel, `delay` frames long.
    rings: [Vec<f32>; 2],
    pos: usize,
}

impl LatencyDelay {
    /// A delay of `frames` frames (allocates; create it off the audio thread).
    pub fn new(frames: u32) -> Self {
        let n = frames.max(1) as usize;
        Self {
            rings: [vec![0.0; n], vec![0.0; n]],
            pos: 0,
        }
    }
}

impl AudioNode for LatencyDelay {
    fn num_inputs(&self) -> usize {
        2
    }

    fn num_outputs(&self) -> usize {
        2
    }

    fn prepare(&mut self, _sample_rate: f32, _max_block: usize) {}

    fn process(&mut self, _ctx: &ProcessContext, io: &mut AudioIo, _events: &[Event]) {
        let frames = io.frames();
        let len = self.rings[0].len();
        for ch in 0..2 {
            let mut pos = self.pos;
            for i in 0..frames {
                let x = io.input(ch).get(i).copied().unwrap_or(0.0);
                let ring = &mut self.rings[ch];
                let y = ring.get(pos).copied().unwrap_or(0.0);
                if let Some(slot) = ring.get_mut(pos) {
                    *slot = x;
                }
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = y;
                }
                pos += 1;
                if pos >= len {
                    pos = 0;
                }
            }
        }
        self.pos = (self.pos + frames) % len.max(1);
    }

    fn reset(&mut self) {
        for r in &mut self.rings {
            r.fill(0.0);
        }
    }

    fn latency(&self) -> u32 {
        self.rings[0].len() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use musiton_core::{TempoMap, TimeSig};

    #[test]
    fn delays_by_exactly_the_given_frames_across_blocks() {
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
        let mut d = LatencyDelay::new(100);
        let mut out = Vec::new();
        for b in 0..5 {
            let input: Vec<f32> = (0..64).map(|i| (b * 64 + i) as f32).collect();
            let ins = vec![input.clone(), input];
            let mut outs = vec![vec![0.0; 64]; 2];
            d.process(&ctx, &mut AudioIo::new(&ins, &mut outs, 64), &[]);
            out.extend_from_slice(&outs[0]);
        }
        assert!(out[..100].iter().all(|s| *s == 0.0));
        for (i, s) in out[100..].iter().enumerate() {
            assert_eq!(*s, i as f32);
        }
    }
}
