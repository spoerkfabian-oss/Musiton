//! Summing point (e.g. the master): the graph sums all connections into its inputs.

use crate::node::{AudioIo, AudioNode, Event, ProcessContext};

/// Passes its (already summed) inputs through unchanged.
#[derive(Debug)]
pub struct Mix {
    channels: usize,
}

impl Mix {
    /// A mix point with `channels` inputs and outputs.
    pub fn new(channels: usize) -> Self {
        Self { channels }
    }
}

impl AudioNode for Mix {
    fn num_inputs(&self) -> usize {
        self.channels
    }

    fn num_outputs(&self) -> usize {
        self.channels
    }

    fn prepare(&mut self, _sample_rate: f32, _max_block: usize) {}

    fn process(&mut self, _ctx: &ProcessContext, io: &mut AudioIo, _events: &[Event]) {
        for ch in 0..self.channels {
            let frames = io.frames();
            for i in 0..frames {
                let x = io.input(ch).get(i).copied().unwrap_or(0.0);
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = x;
                }
            }
        }
    }

    fn reset(&mut self) {}
}
