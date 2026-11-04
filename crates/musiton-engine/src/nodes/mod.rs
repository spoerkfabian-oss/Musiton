//! Nodes that belong to the engine itself.

mod clip_player;
mod fx_slot;
mod metronome;
mod midi_track;
mod mix;
mod record;
mod strip;

pub use clip_player::{ClipPlayer, PlayerClip};
pub use fx_slot::{Analyzer, FxSlot, ANALYZER_SIZE, FX_BYPASS};
pub use metronome::Metronome;
pub use midi_track::{MidiNote, MidiNotes, MidiTrack, RecMidi, MIDI_TRACK_LIVE};
pub use mix::Mix;
pub use record::{AudioInput, RecChunk, Recorder, REC_CHUNK_FRAMES};
pub use strip::{pan_gains, Meter, SendGain, Strip, STRIP_AUDIBLE, STRIP_GAIN, STRIP_PAN};
