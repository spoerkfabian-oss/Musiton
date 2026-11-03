//! Messages from the UI thread to the audio thread.

use musiton_core::ids::ParamId;
use musiton_core::{SamplePos, TempoMap, Tick, TimeSig};

use crate::graph::{CompiledGraph, NodeIdx};

/// Interleaved input samples from the input device's callback.
pub struct InputSource {
    /// Interleaved samples.
    pub samples: rtrb::Consumer<f32>,
    /// Channels per frame.
    pub channels: usize,
}

/// A command for the audio thread. Heap data is boxed so that the audio thread only
/// moves pointers; replaced objects are sent to the GC thread to be freed.
pub enum EngineCommand {
    /// Start the transport.
    Play,
    /// Stop the transport (position stays).
    Stop,
    /// Jump to a position.
    Locate(SamplePos),
    /// Set or clear the loop range (musical, so it follows tempo changes).
    SetLoop(Option<(Tick, Tick)>),
    /// Replace the tempo map; the musical position is kept.
    SetTempoMap(Box<TempoMap>),
    /// Replace the time signature.
    SetTimeSig(TimeSig),
    /// Switch the metronome on/off.
    SetMetronome(bool),
    /// Use a new audio input (or none).
    SetInput(Option<Box<InputSource>>),
    /// Arm/disarm recording (records while the transport runs).
    SetRecording(bool),
    /// Restrict recording to a range (punch in/out); `None` = record everywhere.
    SetPunch(Option<(Tick, Tick)>),
    /// Use a new MIDI input (raw 3-byte messages) or none.
    SetMidiInput(Option<Box<rtrb::Consumer<[u8; 3]>>>),
    /// Play/stop a note on the live-MIDI track (keyboard preview in the editor).
    PreviewNote {
        /// Note on (true) or off.
        on: bool,
        /// Key 0..127.
        key: u8,
        /// Velocity 0..1.
        vel: f32,
    },
    /// Replace the running graph (cross-faded, click-free).
    SwapGraph(Box<CompiledGraph>),
    /// Hand new data to a node of the running graph (e.g. a MIDI track's notes) without
    /// rebuilding the graph; the node keeps its state (sounding voices, filters).
    NodeData {
        /// Node in the running graph.
        node: NodeIdx,
        /// The data; the node decides what it accepts.
        data: Box<dyn std::any::Any + Send>,
    },
    /// Set a node parameter in the running graph.
    SetParam {
        /// Node in the running graph.
        node: NodeIdx,
        /// Parameter.
        id: ParamId,
        /// Value.
        val: f32,
    },
}

/// An object retired by the audio thread, freed on the GC thread.
pub(crate) type Garbage = Box<dyn std::any::Any + Send>;
