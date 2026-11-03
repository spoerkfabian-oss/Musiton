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
    /// Replace the running graph (cross-faded, click-free).
    SwapGraph(Box<CompiledGraph>),
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
pub(crate) type Garbage = Box<dyn Send>;
