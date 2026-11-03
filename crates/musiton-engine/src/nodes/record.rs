//! Live input and recording.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::node::{AudioIo, AudioNode, Event, ProcessContext};

/// Frames per recording chunk.
pub const REC_CHUNK_FRAMES: usize = 256;

/// A piece of recorded stereo audio sent from the audio thread to the disk thread.
/// `len == 0` marks the end of a take.
#[derive(Clone, Copy)]
pub struct RecChunk {
    /// Song position (samples) of the first frame.
    pub pos: u64,
    /// Valid frames in `data`.
    pub len: u16,
    /// Interleaved stereo samples.
    pub data: [f32; REC_CHUNK_FRAMES * 2],
}

impl RecChunk {
    const EMPTY: Self = Self {
        pos: 0,
        len: 0,
        data: [0.0; REC_CHUNK_FRAMES * 2],
    };

    /// End-of-take marker.
    pub fn end_marker() -> Self {
        Self::EMPTY
    }
}

/// Outputs device input channels as stereo (a mono input goes to both sides).
#[derive(Debug, Clone, Copy)]
pub struct AudioInput {
    left: usize,
    right: Option<usize>,
}

impl AudioInput {
    /// Input channel `left` (0-based), plus `right` for a stereo input.
    pub fn new(left: usize, right: Option<usize>) -> Self {
        Self { left, right }
    }
}

impl AudioNode for AudioInput {
    fn num_inputs(&self) -> usize {
        0
    }

    fn num_outputs(&self) -> usize {
        2
    }

    fn prepare(&mut self, _sample_rate: f32, _max_block: usize) {}

    fn process(&mut self, ctx: &ProcessContext, io: &mut AudioIo, _events: &[Event]) {
        let channel = |c: Option<usize>| c.and_then(|c| ctx.inputs.get(c)).map(Vec::as_slice);
        let left = channel(Some(self.left)).unwrap_or(&[]);
        let right = channel(self.right).unwrap_or(left);
        for (ch, src) in [left, right].into_iter().enumerate() {
            let out = io.output(ch);
            for (i, o) in out.iter_mut().enumerate() {
                *o = src.get(i).copied().unwrap_or(0.0);
            }
        }
    }

    fn reset(&mut self) {}
}

/// Sends its stereo input to the disk thread while recording (inside the punch range).
/// A jump in position (loop wrap) or a pause ends the take; the disk thread starts a new
/// file on the next chunk.
pub struct Recorder {
    tx: rtrb::Producer<RecChunk>,
    chunk: Box<RecChunk>,
    active: bool,
    /// Position the next recorded frame must have to continue the current take.
    expected: u64,
    dropped: Arc<AtomicU64>,
}

impl Recorder {
    /// Recorder writing into `tx`; chunks that don't fit are counted in `dropped`.
    pub fn new(tx: rtrb::Producer<RecChunk>, dropped: Arc<AtomicU64>) -> Self {
        Self {
            tx,
            chunk: Box::new(RecChunk::EMPTY),
            active: false,
            expected: 0,
            dropped,
        }
    }

    fn send(&mut self, chunk: RecChunk) {
        if self.tx.push(chunk).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn flush(&mut self) {
        if self.chunk.len > 0 {
            let c = *self.chunk;
            self.send(c);
            self.chunk.len = 0;
        }
    }

    fn end_take(&mut self) {
        if self.active {
            self.flush();
            self.send(RecChunk::end_marker());
            self.active = false;
        }
    }
}

impl AudioNode for Recorder {
    fn num_inputs(&self) -> usize {
        2
    }

    fn num_outputs(&self) -> usize {
        0
    }

    fn prepare(&mut self, _sample_rate: f32, _max_block: usize) {}

    fn process(&mut self, ctx: &ProcessContext, io: &mut AudioIo, _events: &[Event]) {
        for i in 0..ctx.frames {
            let pos = ctx.sample_pos + i as u64;
            let rec = ctx.recording && ctx.punch.is_none_or(|(a, b)| (a..b).contains(&pos));
            if !rec {
                self.end_take();
                continue;
            }
            if self.active && pos != self.expected {
                // Position jumped (loop): the take ends here.
                self.end_take();
            }
            self.active = true;
            self.expected = pos + 1;
            if self.chunk.len == 0 {
                self.chunk.pos = pos;
            }
            let len = usize::from(self.chunk.len);
            for ch in 0..2 {
                let x = io.input(ch).get(i).copied().unwrap_or(0.0);
                if let Some(d) = self.chunk.data.get_mut(len * 2 + ch) {
                    *d = x;
                }
            }
            self.chunk.len += 1;
            if usize::from(self.chunk.len) == REC_CHUNK_FRAMES {
                self.flush();
            }
        }
    }

    fn reset(&mut self) {}
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.end_take();
    }
}
