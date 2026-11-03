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
    ClipPlayer, Meter, Metronome, Mix, PlayerClip, SendGain, Strip, STRIP_AUDIBLE, STRIP_GAIN,
    STRIP_PAN,
};
use musiton_project::{Clip, OutputRoute, Project, TrackKind};

use crate::assets::AssetStore;

/// The parts of a project that need a graph rebuild when they change.
#[derive(Debug, Clone, PartialEq)]
pub struct Structure {
    tracks: Vec<TrackStructure>,
}

/// Rebuild-relevant parts of one track: id, kind, output, sends (target, pre-fader), clips.
type TrackStructure = (
    TrackId,
    TrackKind,
    OutputRoute,
    Vec<(TrackId, bool)>,
    Vec<Clip>,
);

/// Extracts the rebuild-relevant parts of `project`.
pub fn structure(project: &Project) -> Structure {
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
                (t.id, t.kind, t.output, sends, t.clips.clone())
            })
            .collect(),
    }
}

/// A parameter in the running graph: (node, parameter) → value.
pub type ParamKey = (NodeIdx, ParamId);

/// A built graph plus where its mixer parameters and meters are.
pub struct BuiltGraph {
    /// The graph, ready to send to the engine.
    pub graph: CompiledGraph,
    /// Where the mixer parameters and meters of this graph are.
    pub info: GraphInfo,
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
}

/// Mixer parameter values of `project` for the given nodes.
fn params(
    project: &Project,
    strips: &HashMap<Option<TrackId>, NodeIdx>,
    sends: &HashMap<(TrackId, usize), NodeIdx>,
) -> HashMap<ParamKey, f32> {
    let mut out = HashMap::new();
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

impl GraphInfo {
    /// Current parameter values for this graph's nodes.
    pub fn params(&self, project: &Project) -> HashMap<ParamKey, f32> {
        params(project, &self.strips, &self.sends)
    }
}

/// Builds: track sources → strips → buses/master → master strip → output (+ metronome).
pub fn build(
    project: &Project,
    assets: &AssetStore,
    sample_rate: f32,
    max_block: usize,
) -> Result<BuiltGraph, GraphError> {
    let mut b = GraphBuilder::new();
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
                        Clip::Audio(a) => Some(PlayerClip {
                            start: a.start,
                            end: Tick(a.start.0 + a.length.0),
                            source_offset: a.source_offset.0,
                            gain: db_to_gain(a.gain_db),
                            fade_in: a.fade_in,
                            fade_out: a.fade_out,
                            audio: assets.audio(a.asset)?.clone(),
                        }),
                        Clip::Midi(_) => None,
                    })
                    .collect();
                b.add(Box::new(ClipPlayer::new(clips)))
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
        stereo(&mut b, input, 0, strip)?;
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
        },
    })
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
        let g = build(&project, &assets, 48_000.0, 512).unwrap().graph;
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
            build(&project, &assets, 48_000.0, 256).unwrap().graph,
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
            let built = build(project, &assets, 48_000.0, 256).unwrap();
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
}
