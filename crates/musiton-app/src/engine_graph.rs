//! Builds the engine graph from the project and keeps mixer parameters in sync.
//!
//! Structure (clips, routing, sends) → graph rebuild. Fader, pan, mute/solo and send
//! levels → `SetParam` commands, so playing sounds are never interrupted by mixer moves.

use std::collections::HashMap;
use std::sync::Arc;

use musiton_core::ids::{ParamId, TrackId};
use musiton_core::units::db_to_gain;
use musiton_core::Tick;
use musiton_engine::graph::{CompiledGraph, GraphBuilder, GraphError, NodeIdx};
use musiton_engine::nodes::{
    Analyzer, AudioInput, ClipPlayer, FxSlot, Meter, Metronome, MidiNote, MidiTrack, Mix,
    PlayerClip, RecChunk, RecMidi, Recorder, SendGain, Strip, FX_BYPASS, MIDI_TRACK_LIVE,
    STRIP_AUDIBLE, STRIP_GAIN, STRIP_PAN,
};
use musiton_project::{Clip, InputRoute, OutputRoute, Project, RecordSetup, SampleZone, TrackKind};

use crate::assets::AssetStore;

/// The parts of a project that need a graph rebuild when they change.
#[derive(Debug, Clone, PartialEq)]
pub struct Structure {
    tracks: Vec<TrackStructure>,
    input_channels: usize,
}

/// Rebuild-relevant parts of one track: id, kind, output, sends (target, pre-fader), clips.
/// Rebuild-relevant parts of one track: id, kind, output, sends (target, pre-fader),
/// audio clips, record setup, instrument (type, samples). MIDI notes and parameter values
/// are sent to the running graph instead.
type TrackStructure = (
    TrackId,
    TrackKind,
    OutputRoute,
    Vec<(TrackId, bool)>,
    Vec<Clip>,
    RecordSetup,
    Option<(String, Vec<SampleZone>)>,
    Vec<(musiton_core::ids::DeviceId, String)>,
);

/// The effect devices of a track (everything after the instrument).
fn effects_of(t: &musiton_project::Track) -> &[musiton_project::DeviceSlot] {
    let skip = usize::from(t.kind == TrackKind::Instrument && !t.devices.is_empty());
    t.devices.get(skip..).unwrap_or(&[])
}

/// Parameter values of an effect device (settings over defaults) plus its bypass state.
fn effect_params(d: &musiton_project::DeviceSlot) -> Vec<(u64, f32)> {
    let mut out: Vec<(u64, f32)> = musiton_devices::effect_type(d.type_id())
        .map(|info| {
            info.params
                .iter()
                .map(|p| (p.id.0, d.param(p.id.0).unwrap_or(p.default)))
                .collect()
        })
        .unwrap_or_default();
    out.push((FX_BYPASS.0, if d.bypass { 1.0 } else { 0.0 }));
    out
}

/// Type id and samples of a track's instrument (instrument tracks only).
fn instrument_of(t: &musiton_project::Track) -> Option<(String, Vec<SampleZone>)> {
    (t.kind == TrackKind::Instrument).then(|| match t.devices.first() {
        Some(d) => (d.type_id().to_owned(), d.samples().to_vec()),
        None => (musiton_devices::POLYSYNTH.to_owned(), Vec::new()),
    })
}

/// The instrument's parameter values: device settings over type defaults.
fn instrument_params(t: &musiton_project::Track) -> Vec<(u64, f32)> {
    let Some((type_id, _)) = instrument_of(t) else {
        return Vec::new();
    };
    let Some(info) = musiton_devices::instrument_type(&type_id) else {
        return Vec::new();
    };
    let device = t.devices.first();
    info.params
        .iter()
        .map(|p| {
            (
                p.id.0,
                device.and_then(|d| d.param(p.id.0)).unwrap_or(p.default),
            )
        })
        .collect()
}

/// Notes of every instrument track (what its `MidiTrack` node should play).
pub fn all_notes(project: &Project) -> HashMap<TrackId, Vec<MidiNote>> {
    project
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Instrument)
        .map(|t| (t.id, midi_notes(t)))
        .collect()
}

/// Extracts the rebuild-relevant parts of `project` (and the number of input channels).
pub fn structure(project: &Project, input_channels: usize) -> Structure {
    Structure {
        tracks: project
            .tracks
            .iter()
            .map(|t| {
                let sends = t
                    .strip
                    .sends
                    .iter()
                    .map(|s| (s.target, s.pre_fader))
                    .collect();
                let audio_clips = t
                    .clips
                    .iter()
                    .filter(|c| matches!(c, Clip::Audio(_)))
                    .cloned()
                    .collect();
                (
                    t.id,
                    t.kind,
                    t.output,
                    sends,
                    audio_clips,
                    t.record,
                    instrument_of(t),
                    effects_of(t)
                        .iter()
                        .map(|d| (d.id, d.type_id().to_owned()))
                        .collect(),
                )
            })
            .collect(),
        input_channels,
    }
}

/// A parameter in the running graph: (node, parameter) → value.
pub type ParamKey = (NodeIdx, ParamId);

/// Capacity of each recording ring (chunks of 256 frames ≈ 5 s at 48 kHz).
const RECORD_RING_CHUNKS: usize = 1024;

/// The disk side of an armed track's recorder.
pub struct RecordTap {
    /// Which track.
    pub track: TrackId,
    /// Chunks from the recorder node.
    pub rx: rtrb::Consumer<RecChunk>,
    /// 1 = mono, 2 = stereo.
    pub channels: u16,
}

/// A built graph plus where its mixer parameters and meters are.
pub struct BuiltGraph {
    /// The graph, ready to send to the engine.
    pub graph: CompiledGraph,
    /// Where the mixer parameters and meters of this graph are.
    pub info: GraphInfo,
    /// One tap per armed track (empty when nothing is armed or there is no input).
    pub record_taps: Vec<RecordTap>,
    /// Recorded live MIDI of each armed instrument track.
    pub midi_taps: Vec<(TrackId, rtrb::Consumer<RecMidi>)>,
    /// Chunks the disk thread could not take in time.
    pub record_dropped: Arc<std::sync::atomic::AtomicU64>,
}

/// Mixer nodes and meters of a built graph.
#[derive(Default)]
pub struct GraphInfo {
    /// Strip node per track (`None` = master).
    strips: HashMap<Option<TrackId>, NodeIdx>,
    /// Send node per (track, send index).
    sends: HashMap<(TrackId, usize), NodeIdx>,
    /// Level meters per track (`None` = master).
    pub meters: HashMap<Option<TrackId>, Arc<Meter>>,
    /// MIDI track node per instrument track.
    midi: HashMap<TrackId, NodeIdx>,
    /// Effect slot node per effect device.
    fx: HashMap<musiton_core::ids::DeviceId, NodeIdx>,
    /// Spectrum taps of EQ devices.
    pub analyzers: HashMap<musiton_core::ids::DeviceId, Arc<Analyzer>>,
}

/// Mixer parameter values of `project` for the given nodes.
fn params(
    project: &Project,
    strips: &HashMap<Option<TrackId>, NodeIdx>,
    sends: &HashMap<(TrackId, usize), NodeIdx>,
    midi: &HashMap<TrackId, NodeIdx>,
    fx: &HashMap<musiton_core::ids::DeviceId, NodeIdx>,
    live: Option<TrackId>,
) -> HashMap<ParamKey, f32> {
    let mut out = HashMap::new();
    for t in &project.tracks {
        for d in effects_of(t) {
            if let Some(&node) = fx.get(&d.id) {
                for (id, v) in effect_params(d) {
                    out.insert((node, ParamId(id)), v);
                }
            }
        }
    }
    for (track, node) in midi {
        out.insert(
            (*node, MIDI_TRACK_LIVE),
            if live == Some(*track) { 1.0 } else { 0.0 },
        );
    }
    let mut strip = |node: NodeIdx, s: &musiton_project::ChannelStrip, audible: bool| {
        out.insert((node, STRIP_GAIN), db_to_gain(s.gain_db));
        out.insert((node, STRIP_PAN), s.pan);
        out.insert((node, STRIP_AUDIBLE), if audible { 1.0 } else { 0.0 });
    };
    if let Some(&n) = strips.get(&None) {
        strip(n, &project.master, !project.master.mute);
    }
    for t in &project.tracks {
        if let Some(&n) = strips.get(&Some(t.id)) {
            strip(n, &t.strip, project.audible(t.id));
        }
    }
    for t in &project.tracks {
        for (i, s) in t.strip.sends.iter().enumerate() {
            if let Some(&n) = sends.get(&(t.id, i)) {
                out.insert((n, ParamId(0)), db_to_gain(s.level_db));
            }
        }
    }
    out
}

/// Parameter scale used for track-volume automation (dB).
pub const VOLUME_PARAM: musiton_core::params::ParamInfo =
    musiton_core::params::ParamInfo::new(0, "Volume", -60.0, 6.0, 0.0).unit("dB");
/// Parameter scale used for track-pan automation.
pub const PAN_PARAM: musiton_core::params::ParamInfo =
    musiton_core::params::ParamInfo::new(1, "Pan", -1.0, 1.0, 0.0);

/// Everything a track's lanes can automate: (target, display name, parameter scale).
pub fn automation_targets(
    t: &musiton_project::Track,
) -> Vec<(
    musiton_project::ParamTarget,
    String,
    musiton_core::params::ParamInfo,
)> {
    use musiton_project::ParamTarget;
    let mut out = vec![
        (ParamTarget::Volume, "Volume".to_owned(), VOLUME_PARAM),
        (ParamTarget::Pan, "Pan".to_owned(), PAN_PARAM),
    ];
    for (i, d) in t.devices.iter().enumerate() {
        let instrument = i == 0 && t.kind == TrackKind::Instrument;
        let (name, params): (&str, &[musiton_core::params::ParamInfo]) = if instrument {
            musiton_devices::instrument_type(d.type_id()).map_or(("?", &[]), |x| (x.name, x.params))
        } else {
            musiton_devices::effect_type(d.type_id()).map_or(("?", &[]), |x| (x.name, x.params))
        };
        for p in params {
            out.push((
                ParamTarget::Device {
                    device: d.id,
                    param: p.id.0,
                },
                format!("{name}: {}", p.name),
                *p,
            ));
        }
    }
    out
}

impl GraphInfo {
    /// Node and parameter that `target` of `track` controls, with its scale and whether
    /// the node expects a linear gain for a dB value.
    pub fn resolve(
        &self,
        project: &Project,
        track: TrackId,
        target: musiton_project::ParamTarget,
    ) -> Option<(NodeIdx, ParamId, musiton_core::params::ParamInfo, bool)> {
        use musiton_project::ParamTarget;
        let t = project.track(track)?;
        match target {
            ParamTarget::Volume => Some((
                *self.strips.get(&Some(track))?,
                STRIP_GAIN,
                VOLUME_PARAM,
                true,
            )),
            ParamTarget::Pan => {
                Some((*self.strips.get(&Some(track))?, STRIP_PAN, PAN_PARAM, false))
            }
            ParamTarget::Device { device, param } => {
                let info = automation_targets(t)
                    .into_iter()
                    .find(|(x, _, _)| *x == target)
                    .map(|(_, _, i)| i)?;
                let is_instrument = t.kind == TrackKind::Instrument
                    && t.devices.first().is_some_and(|d| d.id == device);
                let node = if is_instrument {
                    self.midi.get(&track)?
                } else {
                    self.fx.get(&device)?
                };
                Some((*node, ParamId(param), info, false))
            }
        }
    }

    /// Engine automation for all lanes that play back, except those in `skip`
    /// (lanes being recorded right now).
    pub fn automation(
        &self,
        project: &Project,
        skip: &[(TrackId, musiton_project::ParamTarget)],
    ) -> musiton_engine::automation::Automation {
        let mut lanes = Vec::new();
        for t in &project.tracks {
            for lane in &t.automation {
                if lane.mode == musiton_project::AutoMode::Off
                    || lane.points.is_empty()
                    || skip.contains(&(t.id, lane.target))
                {
                    continue;
                }
                if let Some((node, param, info, db_to_gain)) =
                    self.resolve(project, t.id, lane.target)
                {
                    lanes.push(musiton_engine::automation::AutoLane {
                        node,
                        param,
                        info,
                        db_to_gain,
                        points: lane
                            .points
                            .iter()
                            .map(|p| (p.tick.0 as f64, p.value, p.curve))
                            .collect(),
                    });
                }
            }
        }
        musiton_engine::automation::Automation(lanes)
    }

    /// The `MidiTrack` node of an instrument track.
    pub fn midi_node(&self, track: TrackId) -> Option<NodeIdx> {
        self.midi.get(&track).copied()
    }

    /// Current parameter values for this graph's nodes; `live` receives the live MIDI.
    pub fn params(&self, project: &Project, live: Option<TrackId>) -> HashMap<ParamKey, f32> {
        params(
            project,
            &self.strips,
            &self.sends,
            &self.midi,
            &self.fx,
            live,
        )
    }
}

/// Builds: track sources → strips → buses/master → master strip → output (+ metronome).
/// `input_channels` is the number of channels of the input device (0 = no input).
pub fn build(
    project: &Project,
    assets: &AssetStore,
    sample_rate: f32,
    max_block: usize,
    input_channels: usize,
) -> Result<BuiltGraph, GraphError> {
    let mut b = GraphBuilder::new();
    let mut record_taps = Vec::new();
    let mut midi_taps = Vec::new();
    let mut midi = HashMap::new();
    let mut fx = HashMap::new();
    let mut analyzers = HashMap::new();
    let record_dropped = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut strips = HashMap::new();
    let mut sends = HashMap::new();
    let mut meters = HashMap::new();

    let out = b.add(Box::new(Mix::new(2)));
    let metro = b.add(Box::new(Metronome::new()));
    b.connect(metro, 0, out, 0)?;
    b.connect(metro, 0, out, 1)?;

    let master_in = b.add(Box::new(Mix::new(2)));
    let master_meter = Arc::new(Meter::default());
    let m = &project.master;
    let master = b.add(Box::new(Strip::new(
        db_to_gain(m.gain_db),
        m.pan,
        !m.mute,
        master_meter.clone(),
    )));
    stereo(&mut b, master_in, 0, master)?;
    stereo(&mut b, master, 0, out)?;
    strips.insert(None, master);
    meters.insert(None, master_meter);

    // Pass 1: an input node and a strip per track.
    let mut inputs = HashMap::new();
    let mut track_strips = HashMap::new();
    for t in project
        .tracks
        .iter()
        .filter(|t| t.kind != TrackKind::Folder)
    {
        let input = match t.kind {
            TrackKind::Audio => {
                let clips: Vec<PlayerClip> = t
                    .clips
                    .iter()
                    .filter_map(|c| match c {
                        Clip::Audio(a) if !a.muted => Some(PlayerClip {
                            start: a.start,
                            end: Tick(a.start.0 + a.length.0),
                            source_offset: a.source_offset.0,
                            gain: db_to_gain(a.gain_db),
                            fade_in: a.fade_in,
                            fade_out: a.fade_out,
                            audio: assets.audio(a.asset)?.clone(),
                        }),
                        _ => None,
                    })
                    .collect();
                b.add(Box::new(ClipPlayer::new(clips)))
            }
            TrackKind::Instrument => {
                let notes = midi_notes(t);
                let rec = t.record.armed.then(|| {
                    let (tx, rx) = rtrb::RingBuffer::new(4096);
                    midi_taps.push((t.id, rx));
                    tx
                });
                let (type_id, zones) = instrument_of(t).unwrap_or_default();
                let samples: Vec<(u8, Arc<musiton_core::audio::AudioData>)> = zones
                    .iter()
                    .filter_map(|z| Some((z.key, assets.audio(z.asset)?.clone())))
                    .collect();
                let instrument =
                    musiton_devices::create_instrument(&type_id, &instrument_params(t), &samples);
                let node = b.add(Box::new(MidiTrack::new(notes, instrument, rec)));
                midi.insert(t.id, node);
                node
            }
            _ => b.add(Box::new(Mix::new(2))),
        };
        let meter = Arc::new(Meter::default());
        let s = &t.strip;
        let strip = b.add(Box::new(Strip::new(
            db_to_gain(s.gain_db),
            s.pan,
            project.audible(t.id),
            meter.clone(),
        )));
        // Source (and live input) → head → effects → strip.
        let head = if t.kind == TrackKind::Bus {
            input
        } else {
            let head = b.add(Box::new(Mix::new(2)));
            stereo(&mut b, input, 0, head)?;
            head
        };
        let mut last = head;
        for d in effects_of(t) {
            let (effect, analyzer) = musiton_devices::create_effect(d.type_id(), &effect_params(d));
            let node = b.add(Box::new(FxSlot::new(effect, d.bypass)));
            stereo(&mut b, last, 0, node)?;
            fx.insert(d.id, node);
            if let Some(a) = analyzer {
                analyzers.insert(d.id, a);
            }
            last = node;
        }
        stereo(&mut b, last, 0, strip)?;
        if t.kind == TrackKind::Audio && input_channels > 0 {
            let rec = t.record;
            let (left, right, channels) = match rec.input {
                InputRoute::Mono(c) => (usize::from(c), None, 1),
                InputRoute::Stereo(c) => (usize::from(c), Some(usize::from(c) + 1), 2),
            };
            let valid = left < input_channels && right.is_none_or(|r| r < input_channels);
            if valid && (rec.armed || rec.monitor.active(rec.armed)) {
                let live = b.add(Box::new(AudioInput::new(left, right)));
                if rec.monitor.active(rec.armed) {
                    stereo(&mut b, live, 0, head)?;
                }
                if rec.armed {
                    let (tx, rx) = rtrb::RingBuffer::new(RECORD_RING_CHUNKS);
                    let recorder = b.add(Box::new(Recorder::new(tx, record_dropped.clone())));
                    stereo(&mut b, live, 0, recorder)?;
                    record_taps.push(RecordTap {
                        track: t.id,
                        rx,
                        channels,
                    });
                }
            }
        }
        inputs.insert(t.id, input);
        track_strips.insert(t.id, strip);
        strips.insert(Some(t.id), strip);
        meters.insert(Some(t.id), meter);
    }

    // Pass 2: outputs and sends (only to existing buses, never into a loop).
    let bus_input = |id: TrackId, from: TrackId| {
        let is_bus = project.track(id).is_some_and(|t| t.kind == TrackKind::Bus);
        (is_bus && id != from && !project.feeds(id, from))
            .then(|| inputs.get(&id).copied())
            .flatten()
    };
    for t in project
        .tracks
        .iter()
        .filter(|t| t.kind != TrackKind::Folder)
    {
        let Some(&strip) = track_strips.get(&t.id) else {
            continue;
        };
        let dest = match t.output {
            OutputRoute::Bus(id) => bus_input(id, t.id).unwrap_or(master_in),
            OutputRoute::Master => master_in,
        };
        stereo(&mut b, strip, 0, dest)?;
        for (i, s) in t.strip.sends.iter().enumerate() {
            let Some(dest) = bus_input(s.target, t.id) else {
                continue;
            };
            let send = b.add(Box::new(SendGain::new(db_to_gain(s.level_db))));
            stereo(&mut b, strip, if s.pre_fader { 2 } else { 0 }, send)?;
            stereo(&mut b, send, 0, dest)?;
            sends.insert((t.id, i), send);
        }
    }

    b.set_output(out);
    Ok(BuiltGraph {
        graph: b.build(sample_rate, max_block)?,
        info: GraphInfo {
            strips,
            sends,
            meters,
            midi,
            fx,
            analyzers,
        },
        record_taps,
        midi_taps,
        record_dropped,
    })
}

/// All notes of a track's (unmuted) MIDI clips on the timeline, cut to their clips.
fn midi_notes(t: &musiton_project::Track) -> Vec<MidiNote> {
    let mut out = Vec::new();
    for c in &t.clips {
        let Clip::Midi(m) = c else {
            continue;
        };
        let clip_end = m.start.0 + m.length.0;
        for n in &m.notes {
            let start = m.start.0 + n.start.0;
            let end = (start + n.length.0).min(clip_end);
            if n.start.0 < 0 || start >= clip_end || end <= start {
                continue;
            }
            out.push(MidiNote {
                start: Tick(start),
                end: Tick(end),
                key: n.pitch.min(127),
                vel: f32::from(n.velocity.min(127)) / 127.0,
                ch: n.channel.min(15),
            });
        }
    }
    out
}

/// Connects output channels `first`, `first + 1` of `src` to inputs 0, 1 of `dst`.
fn stereo(
    b: &mut GraphBuilder,
    src: NodeIdx,
    first: usize,
    dst: NodeIdx,
) -> Result<(), GraphError> {
    b.connect(src, first, dst, 0)?;
    b.connect(src, first + 1, dst, 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use musiton_core::audio::AudioData;
    use musiton_core::ids::{AssetId, ClipId, TrackId};
    use musiton_core::{SamplePos, TempoMap, Tick, PPQ};
    use musiton_engine::{create, render_offline, EngineCommand};
    use musiton_project::{AudioClip, Track};

    fn clip(id: u64, asset: u64, start: i64, len: i64) -> Clip {
        Clip::Audio(AudioClip {
            id: ClipId(id),
            asset: AssetId(asset),
            start: Tick(start),
            length: Tick(len),
            source_offset: SamplePos(0),
            gain_db: 0.0,
            fade_in: Tick(0),
            fade_out: Tick(0),
            muted: false,
        })
    }

    /// A loop-based arrangement: a "kick" on every beat of track 1, a "hat" on the
    /// off-beats of track 2, at 120 BPM. Every sample must land exactly where expected.
    #[test]
    fn arrangement_plays_back_sample_accurately() {
        let kick: Vec<f32> = (0..2_000).map(|i| 1.0 - i as f32 / 2_000.0).collect();
        let hat: Vec<f32> = (0..500)
            .map(|i| if i % 2 == 0 { 0.25 } else { -0.25 })
            .collect();
        let mut assets = AssetStore::default();
        assets.insert(
            AssetId(1),
            AudioData {
                sample_rate: 48_000,
                channels: vec![kick.clone()],
            },
        );
        assets.insert(
            AssetId(2),
            AudioData {
                sample_rate: 48_000,
                channels: vec![hat.clone(), hat.clone()],
            },
        );

        let mut project = Project {
            metronome: false,
            tempo: TempoMap::constant(120.0),
            ..Project::default()
        };
        let beat = PPQ;
        project.tracks.push(Track {
            id: TrackId(10),
            clips: (0..8)
                .map(|k| clip(100 + k, 1, k as i64 * beat, beat))
                .collect(),
            ..Track::default()
        });
        project.tracks.push(Track {
            id: TrackId(11),
            clips: (0..8)
                .map(|k| clip(200 + k, 2, k as i64 * beat + beat / 2, beat / 2))
                .collect(),
            ..Track::default()
        });

        let (mut h, mut e) = create(48_000.0, 512);
        let g = build(&project, &assets, 48_000.0, 512, 0).unwrap().graph;
        h.send(EngineCommand::SwapGraph(Box::new(g))).ok();
        render_offline(&mut e, 2_048, 2, 512); // graph fade-in while stopped
        h.send(EngineCommand::SetTempoMap(Box::new(project.tempo.clone())))
            .ok();
        h.send(EngineCommand::Play).ok();
        let out = render_offline(&mut e, 8 * 24_000, 2, 333);

        let mut expected = vec![0.0_f32; 8 * 24_000];
        for k in 0..8 {
            for (i, s) in kick.iter().enumerate() {
                expected[k * 24_000 + i] += s;
            }
            for (i, s) in hat.iter().enumerate() {
                expected[k * 24_000 + 12_000 + i] += s;
            }
        }
        // Track and master strips are centred: −3 dB each.
        let g = 0.5_f32;
        for (i, f) in out.chunks(2).enumerate() {
            let e = expected[i] * g;
            assert!(
                (f[0] - e).abs() < 1e-6 && (f[1] - e).abs() < 1e-6,
                "sample {i}: {f:?} vs {e}"
            );
        }
        // Samples outside the clips are exactly silent.
        assert_eq!(out[2 * 23_999], 0.0);
    }

    #[test]
    fn muted_and_unsoloed_tracks_are_silent() {
        let mut assets = AssetStore::default();
        assets.insert(
            AssetId(1),
            AudioData {
                sample_rate: 48_000,
                channels: vec![vec![1.0; 1000]],
            },
        );
        let mut project = Project {
            metronome: false,
            ..Project::default()
        };
        for (id, mute, solo) in [(1, true, false), (2, false, false), (3, false, true)] {
            let mut t = Track {
                id: TrackId(id),
                clips: vec![clip(id, 1, 0, PPQ)],
                ..Track::default()
            };
            t.strip.mute = mute;
            t.strip.solo = solo;
            project.tracks.push(t);
        }
        let (mut h, mut e) = create(48_000.0, 256);
        h.send(EngineCommand::SwapGraph(Box::new(
            build(&project, &assets, 48_000.0, 256, 0).unwrap().graph,
        )))
        .ok();
        render_offline(&mut e, 1024, 2, 256);
        h.send(EngineCommand::Play).ok();
        let out = render_offline(&mut e, 100, 2, 100);
        // Only the soloed track plays: 1.0 through two centred strips (−3 dB each).
        assert!(
            out.iter().all(|&s| (s - 0.5).abs() < 1e-6),
            "{:?}",
            &out[..4]
        );
    }

    /// Drums → drum bus, plus a post-fader send at −6 dB to an "FX" bus. Checks the summed
    /// output and that the meters show the offline-computed levels.
    #[test]
    fn drum_bus_with_send() {
        use musiton_project::{OutputRoute, SendRoute};
        let mut assets = AssetStore::default();
        assets.insert(
            AssetId(1),
            AudioData {
                sample_rate: 48_000,
                channels: vec![vec![1.0; 96_000]],
            },
        );
        let mut project = Project {
            metronome: false,
            ..Project::default()
        };
        let mut drums = Track {
            id: TrackId(1),
            clips: vec![clip(5, 1, 0, 8 * PPQ)],
            output: OutputRoute::Bus(TrackId(2)),
            ..Track::default()
        };
        drums.strip.sends.push(SendRoute {
            target: TrackId(3),
            level_db: -6.0,
            pre_fader: false,
        });
        let bus = |id| Track {
            id: TrackId(id),
            kind: TrackKind::Bus,
            ..Track::default()
        };
        project.tracks = vec![drums, bus(2), bus(3)];

        let run = |project: &Project| {
            let built = build(project, &assets, 48_000.0, 256, 0).unwrap();
            let (mut h, mut e) = create(48_000.0, 256);
            h.send(EngineCommand::SwapGraph(Box::new(built.graph))).ok();
            render_offline(&mut e, 1024, 2, 256);
            h.send(EngineCommand::Play).ok();
            (render_offline(&mut e, 96_000, 2, 256), built.info)
        };
        let c = std::f32::consts::FRAC_1_SQRT_2;
        let send = db_to_gain(-6.0);
        let (out, info) = run(&project);
        let expected = c * c * c + c * send * c * c;
        assert!(
            (out[2 * 40_000] - expected).abs() < 1e-5,
            "{} vs {expected}",
            out[2 * 40_000]
        );
        let peak = info.meters[&Some(TrackId(2))].take_peak();
        assert!((peak[0] - c * c).abs() < 1e-5, "drum bus peak {peak:?}");
        let fx = info.meters[&Some(TrackId(3))].take_peak();
        assert!((fx[0] - c * send * c).abs() < 1e-5, "fx bus peak {fx:?}");
        let rms = info.meters[&None].rms();
        assert!((rms[0] - expected).abs() < 1e-3, "master rms {rms:?}");

        // Pre-fader send keeps sending when the drum fader is all the way down.
        let d = project.track_mut(TrackId(1)).unwrap();
        d.strip.gain_db = -200.0;
        d.strip.sends[0].pre_fader = true;
        let (out, _) = run(&project);
        let expected = send * c * c;
        assert!(
            (out[2 * 40_000] - expected).abs() < 1e-5,
            "{} vs {expected}",
            out[2 * 40_000]
        );
    }

    /// Plays notes live into an armed instrument track while recording, turns the recording
    /// into a clip, plays the clip back and compares the audio sample by sample.
    #[test]
    fn recorded_midi_plays_back_identically() {
        use musiton_project::{RecordSetup, TrackKind, UndoStack};
        let assets = AssetStore::default();
        let mut project = Project {
            metronome: false,
            tempo: TempoMap::constant(120.0),
            ..Project::default()
        };
        project.tracks.push(Track {
            id: TrackId(1),
            kind: TrackKind::Instrument,
            record: RecordSetup {
                armed: true,
                ..RecordSetup::default()
            },
            ..Track::default()
        });
        // 250 frames = exactly 10 ticks at 120 BPM / 48 kHz, so the live notes fall on
        // ticks and the stored clip can reproduce them sample-exactly. (Notes are stored
        // with tick resolution: 1 tick = 25 samples here.)
        let block = 250;
        let start = |project: &Project, live: bool| {
            let mut built = build(project, &assets, 48_000.0, block, 0).unwrap();
            let (mut h, mut e) = create(48_000.0, block);
            let params = built.info.params(project, live.then_some(TrackId(1)));
            h.send(EngineCommand::SwapGraph(Box::new(built.graph))).ok();
            for ((node, id), val) in params {
                h.send(EngineCommand::SetParam { node, id, val }).ok();
            }
            render_offline(&mut e, 2048, 2, block);
            let taps = std::mem::take(&mut built.midi_taps);
            (h, e, taps)
        };

        // Take 1: live playing while recording. Notes change on block boundaries.
        let (mut h, mut e, mut taps) = start(&project, true);
        h.send(EngineCommand::SetRecording(true)).ok();
        h.send(EngineCommand::Play).ok();
        let script = [
            (10, true, 60),
            (100, true, 64),
            (180, false, 60),
            (300, false, 64),
            (320, true, 67),
            (500, false, 67),
        ];
        let mut live = Vec::new();
        for b in 0..600 {
            for &(at, on, key) in &script {
                if at == b {
                    h.send(EngineCommand::PreviewNote { on, key, vel: 1.0 })
                        .ok();
                }
            }
            live.extend(render_offline(&mut e, block, 2, block));
        }
        let (_, rx) = &mut taps[0];
        let events: Vec<_> = std::iter::from_fn(|| rx.pop().ok()).collect();
        assert_eq!(events.len(), script.len());

        // Turn it into a clip and play it back.
        let edit = crate::midi_record::to_edit(
            &mut project,
            TrackId(1),
            &events,
            0,
            600 * block as u64,
            48_000.0,
            true,
        )
        .unwrap();
        UndoStack::new().perform(&mut project, edit, false).unwrap();
        project.tracks[0].record.armed = false;
        let (mut h, mut e, _) = start(&project, false);
        h.send(EngineCommand::Play).ok();
        let played = render_offline(&mut e, 600 * block, 2, block);

        assert!(live.iter().any(|&s| s != 0.0));
        let worst = live
            .iter()
            .zip(&played)
            .fold(0.0_f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(worst < 1e-6, "playback differs by {worst}");
    }

    /// Phase 6/10 acceptance: a drum bus with a send to an FX bus that holds a reverb.
    /// After the drum hit ends, the reverb tail keeps sounding; bypassing the reverb
    /// (as a parameter, no rebuild) removes the tail.
    #[test]
    fn drum_bus_with_reverb_send() {
        use musiton_core::ids::DeviceId;
        use musiton_project::{DeviceSlot, OutputRoute, SendRoute};
        let mut assets = AssetStore::default();
        let hit: Vec<f32> = (0..2_000)
            .map(|i| (i as f32 * 0.3).sin() * (1.0 - i as f32 / 2_000.0))
            .collect();
        assets.insert(
            AssetId(1),
            AudioData {
                sample_rate: 48_000,
                channels: vec![hit],
            },
        );
        let mut project = Project {
            metronome: false,
            ..Project::default()
        };
        let mut drums = Track {
            id: TrackId(1),
            clips: vec![clip(5, 1, 0, PPQ)],
            output: OutputRoute::Bus(TrackId(2)),
            ..Track::default()
        };
        drums.strip.sends.push(SendRoute {
            target: TrackId(3),
            level_db: 0.0,
            pre_fader: false,
        });
        let mut fx = Track {
            id: TrackId(3),
            kind: TrackKind::Bus,
            ..Track::default()
        };
        let mut reverb = DeviceSlot::builtin(DeviceId(9), "reverb");
        reverb.set_param(musiton_devices::effects::reverb::id::MIX, Some(1.0));
        fx.devices.push(reverb);
        project.tracks = vec![
            drums,
            Track {
                id: TrackId(2),
                kind: TrackKind::Bus,
                ..Track::default()
            },
            fx,
        ];

        let built = build(&project, &assets, 48_000.0, 256, 0).unwrap();
        let params = built.info.params(&project, None);
        let (mut h, mut e) = create(48_000.0, 256);
        h.send(EngineCommand::SwapGraph(Box::new(built.graph))).ok();
        render_offline(&mut e, 1024, 2, 256);
        h.send(EngineCommand::Play).ok();
        let out = render_offline(&mut e, 24_000, 2, 256);
        let tail = |x: &[f32]| x.chunks(2).map(|f| f[0].abs()).fold(0.0_f32, f32::max);
        // The hit lasts 2 000 samples; the reverb is still audible well after it.
        assert!(tail(&out[2 * 6_000..2 * 12_000]) > 1e-3, "no reverb tail");

        // Bypass the reverb through its parameter and play again from the start.
        let reverb_node = built.info.fx[&DeviceId(9)];
        h.send(EngineCommand::SetParam {
            node: reverb_node,
            id: FX_BYPASS,
            val: 1.0,
        })
        .ok();
        h.send(EngineCommand::Locate(SamplePos(0))).ok();
        render_offline(&mut e, 48_000, 2, 256);
        h.send(EngineCommand::Locate(SamplePos(0))).ok();
        let out = render_offline(&mut e, 24_000, 2, 256);
        assert!(
            tail(&out[2 * 6_000..2 * 12_000]) < 1e-6,
            "bypassed reverb still sounds"
        );
        assert!(params.contains_key(&(reverb_node, FX_BYPASS)));
    }

    /// An EQ in an audio track's chain feeds its analyzer with what plays through it.
    #[test]
    fn eq_analyzer_sees_the_track_audio() {
        use musiton_core::ids::DeviceId;
        use musiton_project::DeviceSlot;
        let mut assets = AssetStore::default();
        let tone: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.1).sin() * 0.5).collect();
        assets.insert(
            AssetId(1),
            AudioData {
                sample_rate: 48_000,
                channels: vec![tone],
            },
        );
        let mut project = Project {
            metronome: false,
            ..Project::default()
        };
        let mut t = Track {
            id: TrackId(1),
            clips: vec![clip(5, 1, 0, 4 * PPQ)],
            ..Track::default()
        };
        t.devices.push(DeviceSlot::builtin(DeviceId(9), "eq"));
        project.tracks.push(t);
        let built = build(&project, &assets, 48_000.0, 256, 0).unwrap();
        let analyzer = built.info.analyzers[&DeviceId(9)].clone();
        let (mut h, mut e) = create(48_000.0, 256);
        h.send(EngineCommand::SwapGraph(Box::new(built.graph))).ok();
        render_offline(&mut e, 1024, 2, 256);
        h.send(EngineCommand::Play).ok();
        render_offline(&mut e, 8192, 2, 256);
        let latest = analyzer.latest(4096);
        assert!(latest.iter().any(|s| s.abs() > 0.1), "analyzer is silent");
    }

    /// Phase 11 acceptance: a filter sweep drawn as automation renders identically every
    /// time — with different callback sizes, and against the stored golden render.
    #[test]
    fn automated_filter_sweep_renders_identically() {
        use musiton_core::ids::DeviceId;
        use musiton_devices::polysynth::id;
        use musiton_project::{
            AutoMode, AutoPoint, AutomationLane, DeviceSlot, MidiClip, Note, ParamTarget, TrackKind,
        };
        let assets = AssetStore::default();
        let mut project = Project {
            metronome: false,
            tempo: TempoMap::constant(120.0),
            ..Project::default()
        };
        let mut synth = DeviceSlot::builtin(DeviceId(7), musiton_devices::POLYSYNTH);
        synth.set_param(id::RESONANCE, Some(0.6));
        let notes = vec![Note {
            pitch: 45,
            velocity: 110,
            start: Tick(0),
            length: Tick(8 * PPQ),
            channel: 0,
        }];
        project.tracks.push(Track {
            id: TrackId(1),
            kind: TrackKind::Instrument,
            devices: vec![synth],
            clips: vec![Clip::Midi(MidiClip {
                id: ClipId(3),
                start: Tick(0),
                length: Tick(8 * PPQ),
                notes,
            })],
            automation: vec![AutomationLane {
                target: ParamTarget::Device {
                    device: DeviceId(7),
                    param: id::CUTOFF,
                },
                points: vec![
                    AutoPoint {
                        tick: Tick(0),
                        value: 0.2,
                        curve: 0.0,
                    },
                    AutoPoint {
                        tick: Tick(4 * PPQ),
                        value: 0.9,
                        curve: 0.5,
                    },
                    AutoPoint {
                        tick: Tick(8 * PPQ),
                        value: 0.3,
                        curve: -0.5,
                    },
                ],
                mode: AutoMode::Read,
            }],
            ..Track::default()
        });
        let render = |project: &Project, block: usize, automate: bool| {
            let built = build(project, &assets, 48_000.0, 512, 0).unwrap();
            let auto = built.info.automation(project, &[]);
            assert_eq!(auto.0.len(), 1);
            let params = built.info.params(project, None);
            let (mut h, mut e) = create(48_000.0, 512);
            h.send(EngineCommand::SwapGraph(Box::new(built.graph))).ok();
            for ((node, pid), val) in params {
                h.send(EngineCommand::SetParam { node, id: pid, val }).ok();
            }
            if automate {
                h.send(EngineCommand::SetAutomation(Some(Box::new(auto))))
                    .ok();
            }
            render_offline(&mut e, 1024, 2, 512);
            h.send(EngineCommand::Play).ok();
            render_offline(&mut e, 4 * 24_000, 2, block)
        };
        let a = render(&project, 256, true);
        let b = render(&project, 100, true);
        let c = render(&project, 333, true);
        assert_eq!(a, b, "different callback size changed the render");
        assert_eq!(a, c);
        assert_ne!(a, render(&project, 256, false), "automation had no effect");

        let left: Vec<f32> = a.chunks(2).map(|f| f[0]).collect();
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden/filter_sweep.wav");
        if std::env::var_os("MUSITON_BLESS").is_some() {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 48_000,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            };
            let mut w = hound::WavWriter::create(&path, spec).unwrap();
            for s in &left {
                w.write_sample(*s).unwrap();
            }
            w.finalize().unwrap();
        } else {
            let mut r = hound::WavReader::open(&path)
                .expect("golden file missing (run with MUSITON_BLESS=1)");
            let golden: Vec<f32> = r.samples::<f32>().map(Result::unwrap).collect();
            assert_eq!(golden.len(), left.len());
            let worst = golden
                .iter()
                .zip(&left)
                .fold(0.0_f32, |m, (g, x)| m.max((g - x).abs()));
            assert!(worst <= 1e-4, "differs from golden by {worst}");
        }
    }
}
