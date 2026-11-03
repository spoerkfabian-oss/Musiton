//! The [`AudioNode`] trait and what a node sees while processing.

use musiton_core::ids::ParamId;
use musiton_core::{TempoMap, TimeSig};

/// Everything a node may know about the current block. Positions refer to the first frame.
pub struct ProcessContext<'a> {
    /// Sample rate in Hz.
    pub sample_rate: f32,
    /// Number of frames in this block (≤ the `max_block` given to `prepare`).
    pub frames: usize,
    /// Whether the transport is running.
    pub playing: bool,
    /// Song position of the first frame, in samples.
    pub sample_pos: u64,
    /// Current tempo map.
    pub tempo: &'a TempoMap,
    /// Current time signature.
    pub time_sig: TimeSig,
    /// Whether the metronome is switched on.
    pub metronome: bool,
    /// Audio input channels for this block (from the input device; may be empty).
    pub inputs: &'a [Vec<f32>],
    /// Whether recording is active (transport running and record enabled).
    pub recording: bool,
    /// Punch range in samples: recording only happens inside it.
    pub punch: Option<(u64, u64)>,
    /// Live MIDI received since the last block (keyboard, note preview), offset 0.
    pub live_midi: &'a [Event],
}

impl EventKind {
    /// Parses a raw 3-byte MIDI channel message (note on/off, CC). Others → `None`.
    pub fn from_midi(bytes: [u8; 3]) -> Option<Self> {
        let [status, d1, d2] = bytes;
        let ch = status & 0x0F;
        let (key, val) = (d1 & 0x7F, d2 & 0x7F);
        match status & 0xF0 {
            0x90 if val > 0 => Some(EventKind::NoteOn {
                ch,
                key,
                vel: f32::from(val) / 127.0,
            }),
            0x80 | 0x90 => Some(EventKind::NoteOff { ch, key }),
            0xB0 if key == 123 => Some(EventKind::AllNotesOff),
            0xB0 => Some(EventKind::Cc {
                ch,
                cc: key,
                val: f32::from(val) / 127.0,
            }),
            _ => None,
        }
    }
}

/// A timed event inside a block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Event {
    /// Frame offset inside the block.
    pub offset: u32,
    /// What happens.
    pub kind: EventKind,
}

/// Kinds of [`Event`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EventKind {
    /// Note starts; velocity 0..1.
    NoteOn {
        /// MIDI channel 0..15.
        ch: u8,
        /// MIDI key 0..127.
        key: u8,
        /// Velocity 0..1.
        vel: f32,
    },
    /// Note ends.
    NoteOff {
        /// MIDI channel 0..15.
        ch: u8,
        /// MIDI key 0..127.
        key: u8,
    },
    /// Controller change; value 0..1.
    Cc {
        /// MIDI channel 0..15.
        ch: u8,
        /// Controller number 0..127.
        cc: u8,
        /// Value 0..1.
        val: f32,
    },
    /// Release every sounding note (transport stopped or jumped).
    AllNotesOff,
    /// Parameter change (normalized 0..1 or plain, per node).
    Param {
        /// Which parameter.
        id: ParamId,
        /// New value.
        val: f32,
    },
}

/// Describes one parameter of a node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamInfo {
    /// Parameter id (unique within the node).
    pub id: ParamId,
    /// Display name.
    pub name: &'static str,
    /// Minimum value.
    pub min: f32,
    /// Maximum value.
    pub max: f32,
    /// Default value.
    pub default: f32,
}

/// Input and output channel buffers of one node for one block.
pub struct AudioIo<'a> {
    inputs: &'a [Vec<f32>],
    outputs: &'a mut [Vec<f32>],
    frames: usize,
}

impl<'a> AudioIo<'a> {
    /// Wraps buffers; each must hold at least `frames` samples.
    pub fn new(inputs: &'a [Vec<f32>], outputs: &'a mut [Vec<f32>], frames: usize) -> Self {
        Self {
            inputs,
            outputs,
            frames,
        }
    }

    /// Frames in this block.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Number of input channels.
    pub fn num_inputs(&self) -> usize {
        self.inputs.len()
    }

    /// Number of output channels.
    pub fn num_outputs(&self) -> usize {
        self.outputs.len()
    }

    /// Input channel `ch` (empty if it doesn't exist).
    pub fn input(&self, ch: usize) -> &[f32] {
        self.inputs
            .get(ch)
            .and_then(|b| b.get(..self.frames))
            .unwrap_or(&[])
    }

    /// Output channel `ch` (empty if it doesn't exist).
    pub fn output(&mut self, ch: usize) -> &mut [f32] {
        let frames = self.frames;
        self.outputs
            .get_mut(ch)
            .and_then(|b| b.get_mut(..frames))
            .unwrap_or(&mut [])
    }

    /// All output channels.
    pub fn outputs(&mut self) -> impl Iterator<Item = &mut [f32]> {
        let frames = self.frames;
        self.outputs
            .iter_mut()
            .filter_map(move |b| b.get_mut(..frames))
    }
}

/// A unit of audio processing (instrument, effect, mixer, metronome …).
///
/// `process` runs on the audio thread: no allocation, locks, I/O or panics.
pub trait AudioNode: Send {
    /// Number of input channels.
    fn num_inputs(&self) -> usize;
    /// Number of output channels.
    fn num_outputs(&self) -> usize;
    /// Called before use, off the audio thread. Allocate here.
    fn prepare(&mut self, sample_rate: f32, max_block: usize);
    /// Renders one block.
    fn process(&mut self, ctx: &ProcessContext, io: &mut AudioIo, events: &[Event]);
    /// Clears internal state (tails, voices).
    fn reset(&mut self);
    /// Latency in samples (for delay compensation).
    fn latency(&self) -> u32 {
        0
    }
    /// Parameters this node exposes.
    fn params(&self) -> &[ParamInfo] {
        &[]
    }
    /// Sets a parameter (audio thread).
    fn set_param(&mut self, _id: ParamId, _value: f32) {}
}
