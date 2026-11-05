//! Serializable project data. Every field has a default so newer fields load from older files.

use musiton_core::ids::{AssetId, ClipId, DeviceId, MarkerId, TrackId};
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
    /// Punch-in/out range for recording.
    pub punch_range: Option<(Tick, Tick)>,
    /// Whether recording is limited to the punch range.
    pub punch_enabled: bool,
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
            punch_range: None,
            punch_enabled: false,
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

    /// Bus tracks that `from` sends or outputs to directly.
    fn targets(&self, from: TrackId) -> Vec<TrackId> {
        let Some(t) = self.track(from) else {
            return Vec::new();
        };
        let mut out: Vec<TrackId> = t.strip.sends.iter().map(|s| s.target).collect();
        if let OutputRoute::Bus(b) = t.output {
            out.push(b);
        }
        out
    }

    /// `true` if `to` can be reached from `from` through outputs and sends
    /// (so a new connection `to → from` would create a feedback loop).
    pub fn feeds(&self, from: TrackId, to: TrackId) -> bool {
        let mut stack = vec![from];
        let mut seen = Vec::new();
        while let Some(t) = stack.pop() {
            if t == to {
                return true;
            }
            if seen.contains(&t) {
                continue;
            }
            seen.push(t);
            stack.extend(self.targets(t));
        }
        false
    }

    /// Bus tracks `from` may route to (output or send) without creating a loop.
    pub fn valid_bus_targets(&self, from: TrackId) -> Vec<TrackId> {
        self.tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Bus && t.id != from && !self.feeds(t.id, from))
            .map(|t| t.id)
            .collect()
    }

    /// Whether each track is heard, given mute, solo and solo-safe. A soloed track keeps
    /// the buses it feeds audible, and a soloed bus keeps the tracks feeding it audible.
    pub fn audible(&self, id: TrackId) -> bool {
        let Some(t) = self.track(id) else {
            return false;
        };
        if t.strip.mute {
            return false;
        }
        let soloed: Vec<TrackId> = self
            .tracks
            .iter()
            .filter(|t| t.strip.solo)
            .map(|t| t.id)
            .collect();
        if soloed.is_empty() || t.strip.solo || t.strip.solo_safe {
            return true;
        }
        soloed
            .iter()
            .any(|&s| self.feeds(s, id) || self.feeds(id, s))
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

/// Which device inputs a track records from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputRoute {
    /// One input channel (0-based).
    Mono(u16),
    /// Two input channels starting at this one (0-based).
    Stereo(u16),
}

impl Default for InputRoute {
    fn default() -> Self {
        InputRoute::Mono(0)
    }
}

/// When a track plays its live input.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MonitorMode {
    /// Never.
    Off,
    /// While the track is armed for recording.
    #[default]
    Auto,
    /// Always.
    On,
}

impl MonitorMode {
    /// Whether the live input is heard for a track that is (not) armed.
    pub fn active(self, armed: bool) -> bool {
        match self {
            MonitorMode::Off => false,
            MonitorMode::Auto => armed,
            MonitorMode::On => true,
        }
    }
}

/// Recording settings of a track.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordSetup {
    /// Armed for recording.
    pub armed: bool,
    /// Input monitoring.
    pub monitor: MonitorMode,
    /// Device input(s).
    pub input: InputRoute,
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
    /// Input, arm and monitoring.
    pub record: RecordSetup,
    /// Device chain: the instrument first (instrument tracks), then effects.
    pub devices: Vec<DeviceSlot>,
    /// Automation lanes.
    pub automation: Vec<AutomationLane>,
}

/// What an automation lane controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ParamTarget {
    /// Track volume (fader, dB).
    Volume,
    /// Track pan.
    Pan,
    /// A parameter of a device on the track.
    Device {
        /// The device.
        device: DeviceId,
        /// Parameter id.
        param: u64,
    },
}

/// How a lane behaves during playback.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutoMode {
    /// Ignored.
    Off,
    /// Plays back.
    #[default]
    Read,
    /// While playing, records the control continuously (overwrites).
    Write,
    /// Records only while the control is being moved.
    Touch,
    /// Starts recording when the control is moved and keeps the last value until stop.
    Latch,
}

/// A point on an automation lane.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AutoPoint {
    /// Position.
    pub tick: Tick,
    /// Value, normalized 0..1 (knob position).
    pub value: f32,
    /// Shape of the segment to the next point: 0 = straight, >0 bends late, <0 bends early.
    #[serde(default)]
    pub curve: f32,
}

/// Automation of one parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutomationLane {
    /// What it controls.
    pub target: ParamTarget,
    /// Points sorted by tick.
    pub points: Vec<AutoPoint>,
    /// Playback/recording mode.
    #[serde(default)]
    pub mode: AutoMode,
}

impl AutomationLane {
    /// Normalized value at `tick` (`None` without points).
    pub fn value_at(&self, tick: f64) -> Option<f32> {
        let first = self.points.first()?;
        let i = self.points.partition_point(|p| (p.tick.0 as f64) <= tick);
        if i == 0 {
            return Some(first.value);
        }
        let a = self.points.get(i - 1)?;
        let Some(b) = self.points.get(i) else {
            return Some(a.value);
        };
        let span = (b.tick.0 - a.tick.0).max(1) as f64;
        let x = ((tick - a.tick.0 as f64) / span) as f32;
        Some(a.value + (b.value - a.value) * musiton_core::params::curve_shape(x, a.curve))
    }
}

/// One device in a track's chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceSlot {
    /// Unique id.
    pub id: DeviceId,
    /// Bypassed devices pass audio through unchanged.
    #[serde(default)]
    pub bypass: bool,
    /// What it is.
    pub kind: DeviceKind,
}

/// Device types.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DeviceKind {
    /// A built-in instrument or effect.
    Builtin {
        /// Type id, e.g. "polysynth".
        type_id: String,
        /// Changed parameters: (parameter id, value). Missing ones use the default.
        #[serde(default)]
        params: Vec<(u64, f32)>,
        /// Samples (sampler): one root sample, or one per pad key in drum mode.
        #[serde(default)]
        samples: Vec<SampleZone>,
    },
}

impl DeviceSlot {
    /// A built-in device with default settings.
    pub fn builtin(id: DeviceId, type_id: &str) -> Self {
        Self {
            id,
            bypass: false,
            kind: DeviceKind::Builtin {
                type_id: type_id.to_owned(),
                params: Vec::new(),
                samples: Vec::new(),
            },
        }
    }

    /// The device's type id.
    pub fn type_id(&self) -> &str {
        match &self.kind {
            DeviceKind::Builtin { type_id, .. } => type_id,
        }
    }

    /// Changed parameter values.
    pub fn params(&self) -> &[(u64, f32)] {
        match &self.kind {
            DeviceKind::Builtin { params, .. } => params,
        }
    }

    /// Value of parameter `id`, if it was changed from the default.
    pub fn param(&self, id: u64) -> Option<f32> {
        self.params()
            .iter()
            .find(|(p, _)| *p == id)
            .map(|(_, v)| *v)
    }

    /// Sets (`Some`) or resets to default (`None`) a parameter; returns the old value.
    pub fn set_param(&mut self, id: u64, value: Option<f32>) -> Option<f32> {
        let DeviceKind::Builtin { params, .. } = &mut self.kind;
        let old = params
            .iter()
            .position(|(p, _)| *p == id)
            .map(|i| params.remove(i).1);
        if let Some(v) = value {
            params.push((id, v));
            params.sort_by_key(|(p, _)| *p);
        }
        old
    }

    /// Sample zones.
    pub fn samples(&self) -> &[SampleZone] {
        match &self.kind {
            DeviceKind::Builtin { samples, .. } => samples,
        }
    }
}

/// A sample assigned to a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SampleZone {
    /// Root key (melodic) or pad key (drum mode).
    pub key: u8,
    /// The audio file.
    pub asset: AssetId,
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
            record: RecordSetup::default(),
            devices: Vec::new(),
            automation: Vec::new(),
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
    /// Keeps playing while other tracks are soloed (typical for effect buses).
    pub solo_safe: bool,
    /// Sends to bus tracks.
    pub sends: Vec<SendRoute>,
}

/// A send from a track to a bus (named so it doesn't clash with `std::marker::Send`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SendRoute {
    /// Receiving bus track.
    pub target: TrackId,
    /// Send level in dB.
    pub level_db: f32,
    /// Taken before the fader (true) or after fader and pan (false).
    pub pre_fader: bool,
}

impl Default for SendRoute {
    fn default() -> Self {
        Self {
            target: TrackId(0),
            level_db: -6.0,
            pre_fader: false,
        }
    }
}

impl Default for ChannelStrip {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
            solo_safe: false,
            sends: Vec::new(),
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
    /// Muted clips are shown but not played (older recording takes).
    pub muted: bool,
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
            muted: false,
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
