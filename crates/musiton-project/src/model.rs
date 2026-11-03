//! Serializable project data. Every field has a default so newer fields load from older files.

use musiton_core::ids::{AssetId, ClipId, MarkerId, TrackId};
use musiton_core::{SamplePos, TempoMap, Tick, TimeSig};
use serde::{Deserialize, Serialize};

use crate::file::CURRENT_VERSION;

/// A whole song.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Project {
    /// File format version (see [`crate::migrate`]).
    pub version: u32,
    /// Project sample rate in Hz (imported audio is converted to it).
    pub sample_rate: u32,
    /// Tempo changes.
    pub tempo: TempoMap,
    /// Time signature (one for the whole song for now).
    pub time_sig: TimeSig,
    /// Tracks in display order.
    pub tracks: Vec<Track>,
    /// Master channel.
    pub master: ChannelStrip,
    /// Audio files used by clips (stored in the project's `audio/` folder).
    pub assets: Vec<Asset>,
    /// Song markers.
    pub markers: Vec<Marker>,
    /// Loop range.
    pub loop_range: Option<(Tick, Tick)>,
    /// Whether the loop is switched on.
    pub loop_enabled: bool,
    /// Whether the metronome is switched on.
    pub metronome: bool,
    /// Next free id (shared by all id types).
    pub next_id: u64,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            sample_rate: 48_000,
            tempo: TempoMap::default(),
            time_sig: TimeSig::default(),
            tracks: Vec::new(),
            master: ChannelStrip::default(),
            assets: Vec::new(),
            markers: Vec::new(),
            loop_range: Some((Tick(0), Tick(4 * TimeSig::default().ticks_per_bar()))),
            loop_enabled: false,
            metronome: true,
            next_id: 1,
        }
    }
}

impl Project {
    /// Returns a fresh id number (never reused within this project).
    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Index of the track with `id`.
    pub fn track_index(&self, id: TrackId) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    /// The track with `id`.
    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    /// The track with `id`, mutable.
    pub fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }
}

/// What a track does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    /// Plays/records audio clips.
    #[default]
    Audio,
    /// Plays MIDI clips through an instrument.
    Instrument,
    /// Sums other tracks (group/effect return).
    Bus,
    /// Groups tracks visually.
    Folder,
}

/// Where a track's output goes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputRoute {
    /// The master channel.
    #[default]
    Master,
    /// A bus track.
    Bus(TrackId),
}

/// One track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Track {
    /// Unique id.
    pub id: TrackId,
    /// Display name.
    pub name: String,
    /// Display colour (RGB).
    pub color: [u8; 3],
    /// Track type.
    pub kind: TrackKind,
    /// Clips (not necessarily sorted).
    pub clips: Vec<Clip>,
    /// Volume, pan, mute, solo.
    pub strip: ChannelStrip,
    /// Output routing.
    pub output: OutputRoute,
}

impl Default for Track {
    fn default() -> Self {
        Self {
            id: TrackId(0),
            name: String::new(),
            color: [120, 160, 220],
            kind: TrackKind::Audio,
            clips: Vec::new(),
            strip: ChannelStrip::default(),
            output: OutputRoute::Master,
        }
    }
}

/// Fader section of a track or the master.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelStrip {
    /// Volume in dB.
    pub gain_db: f32,
    /// Pan −1 (left) … +1 (right).
    pub pan: f32,
    /// Muted.
    pub mute: bool,
    /// Soloed.
    pub solo: bool,
}

impl Default for ChannelStrip {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
        }
    }
}

/// A clip on a track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Clip {
    /// Plays part of an audio file.
    Audio(AudioClip),
    /// Contains notes.
    Midi(MidiClip),
}

impl Clip {
    /// The clip's id.
    pub fn id(&self) -> ClipId {
        match self {
            Clip::Audio(c) => c.id,
            Clip::Midi(c) => c.id,
        }
    }

    /// Start position on the timeline.
    pub fn start(&self) -> Tick {
        match self {
            Clip::Audio(c) => c.start,
            Clip::Midi(c) => c.start,
        }
    }

    /// Length on the timeline.
    pub fn length(&self) -> Tick {
        match self {
            Clip::Audio(c) => c.length,
            Clip::Midi(c) => c.length,
        }
    }

    /// End position on the timeline (exclusive).
    pub fn end(&self) -> Tick {
        Tick(self.start().0 + self.length().0)
    }

    /// Gives the clip a new id (for duplicates).
    pub fn set_id(&mut self, id: ClipId) {
        match self {
            Clip::Audio(c) => c.id = id,
            Clip::Midi(c) => c.id = id,
        }
    }

    /// Moves the clip to `start`.
    pub fn set_start(&mut self, start: Tick) {
        match self {
            Clip::Audio(c) => c.start = start,
            Clip::Midi(c) => c.start = start,
        }
    }
}

/// Audio clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioClip {
    /// Unique id.
    pub id: ClipId,
    /// Which audio file.
    pub asset: AssetId,
    /// Timeline start.
    pub start: Tick,
    /// Length on the timeline.
    pub length: Tick,
    /// Where in the file playback starts.
    pub source_offset: SamplePos,
    /// Clip volume in dB.
    pub gain_db: f32,
    /// Fade-in length.
    pub fade_in: Tick,
    /// Fade-out length.
    pub fade_out: Tick,
}

impl Default for AudioClip {
    fn default() -> Self {
        Self {
            id: ClipId(0),
            asset: AssetId(0),
            start: Tick(0),
            length: Tick(0),
            source_offset: SamplePos(0),
            gain_db: 0.0,
            fade_in: Tick(0),
            fade_out: Tick(0),
        }
    }
}

/// MIDI clip.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MidiClip {
    /// Unique id.
    pub id: ClipId,
    /// Timeline start.
    pub start: Tick,
    /// Length on the timeline.
    pub length: Tick,
    /// Notes (start relative to the clip).
    pub notes: Vec<Note>,
}

/// A MIDI note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    /// Key 0..127.
    pub pitch: u8,
    /// Velocity 1..127.
    pub velocity: u8,
    /// Start relative to the clip.
    pub start: Tick,
    /// Length.
    pub length: Tick,
    /// MIDI channel 0..15.
    pub channel: u8,
}

/// An audio file in the project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    /// Unique id.
    pub id: AssetId,
    /// Path relative to the project folder (e.g. `audio/kick.wav`).
    pub file: String,
}

/// A named position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    /// Unique id.
    pub id: MarkerId,
    /// Position.
    pub tick: Tick,
    /// Label.
    pub name: String,
}
