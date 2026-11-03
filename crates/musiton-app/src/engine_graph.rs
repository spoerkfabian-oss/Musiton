//! Builds the engine graph from the project.

use musiton_core::units::db_to_gain;
use musiton_engine::graph::{CompiledGraph, GraphBuilder, GraphError};
use musiton_engine::nodes::{ClipPlayer, Metronome, Mix, PlayerClip};
use musiton_project::{Clip, Project, TrackKind};

use crate::assets::AssetStore;

/// Metronome → master, every audible audio track's clips → master.
/// Muted tracks (and non-soloed ones while anything is soloed) are left out.
pub fn build(
    project: &Project,
    assets: &AssetStore,
    sample_rate: f32,
    max_block: usize,
) -> Result<CompiledGraph, GraphError> {
    let mut b = GraphBuilder::new();
    let master = b.add(Box::new(Mix::new(2)));
    let metro = b.add(Box::new(Metronome::new()));
    b.connect(metro, 0, master, 0)?;
    b.connect(metro, 0, master, 1)?;

    let any_solo = project.tracks.iter().any(|t| t.strip.solo);
    for track in &project.tracks {
        if track.kind != TrackKind::Audio || track.strip.mute || (any_solo && !track.strip.solo) {
            continue;
        }
        let clips: Vec<PlayerClip> = track
            .clips
            .iter()
            .filter_map(|c| match c {
                Clip::Audio(a) => Some(PlayerClip {
                    start: a.start,
                    end: musiton_core::Tick(a.start.0 + a.length.0),
                    source_offset: a.source_offset.0,
                    gain: db_to_gain(a.gain_db),
                    fade_in: a.fade_in,
                    fade_out: a.fade_out,
                    audio: assets.audio(a.asset)?.clone(),
                }),
                Clip::Midi(_) => None,
            })
            .collect();
        if clips.is_empty() {
            continue;
        }
        let player = b.add(Box::new(ClipPlayer::new(clips)));
        b.connect(player, 0, master, 0)?;
        b.connect(player, 1, master, 1)?;
    }
    b.set_output(master);
    b.build(sample_rate, max_block)
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
        let g = build(&project, &assets, 48_000.0, 512).unwrap();
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
        let left: Vec<f32> = out.chunks(2).map(|f| f[0]).collect();
        let right: Vec<f32> = out.chunks(2).map(|f| f[1]).collect();
        assert_eq!(left, expected);
        assert_eq!(right, expected);
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
            build(&project, &assets, 48_000.0, 256).unwrap(),
        )))
        .ok();
        render_offline(&mut e, 1024, 2, 256);
        h.send(EngineCommand::Play).ok();
        let out = render_offline(&mut e, 100, 2, 100);
        // Only the soloed track plays: level 1.0, not 2.0 or 3.0.
        assert!(out.iter().all(|&s| s == 1.0));
    }
}
