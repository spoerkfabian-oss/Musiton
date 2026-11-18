//! Warped audio clips: renders a clip's audio time-stretched to the song's tempo map.
//!
//! A warped clip's file has its own tempo. For the current tempo map its audio is
//! stretched (pitch kept, WSOLA) into a buffer that starts at the clip's start and plays
//! 1:1 on the timeline. Buffers are rendered off the audio thread and cached per clip,
//! tempo map and sample rate.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use musiton_core::audio::AudioData;
use musiton_core::TempoMap;
use musiton_project::{AudioClip, Clip, Project};

use crate::assets::AssetStore;

/// Identifies one rendering of a warped clip: everything its audio depends on.
pub fn key(clip: &AudioClip, tempo: &TempoMap, rate: u32) -> u64 {
    let mut h = DefaultHasher::new();
    clip.asset.0.hash(&mut h);
    clip.start.0.hash(&mut h);
    clip.length.0.hash(&mut h);
    clip.source_offset.0.hash(&mut h);
    clip.warp.map(|w| w.bpm.to_bits()).hash(&mut h);
    for p in tempo.points() {
        (p.tick.0, p.bpm.to_bits(), p.ramp).hash(&mut h);
    }
    rate.hash(&mut h);
    h.finish()
}

/// The warped audio clips of `project` (unmuted ones only).
pub fn clips(project: &Project) -> Vec<AudioClip> {
    project
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .filter_map(|c| match c {
            Clip::Audio(a) if a.warp.is_some() && !a.muted => Some(a.clone()),
            _ => None,
        })
        .collect()
}

/// A warped clip still to render: its key, the clip and its file.
pub type Job = (u64, AudioClip, Arc<AudioData>);

/// The keys every warped clip of `project` needs at `rate`, and the renderings that are
/// missing from `assets`.
pub fn todo(project: &Project, assets: &AssetStore, rate: u32) -> (HashSet<u64>, Vec<Job>) {
    let mut keys = HashSet::new();
    let mut jobs = Vec::new();
    for clip in clips(project) {
        let k = key(&clip, &project.tempo, rate);
        if keys.insert(k) && assets.warped(k).is_none() {
            if let Some(audio) = assets.audio_at(clip.asset, rate) {
                jobs.push((k, clip, audio.clone()));
            }
        }
    }
    (keys, jobs)
}

/// Renders every missing warped clip of `project` into `assets` right away (export).
pub fn render_missing(project: &Project, assets: &mut AssetStore, rate: u32) {
    for (k, clip, audio) in todo(project, assets, rate).1 {
        assets.insert_warped(k, render(&clip, &audio, &project.tempo, rate));
    }
}

/// Renders `clip` (whose file is `audio`, at `rate`) for `tempo`: the result starts at
/// the clip's start and lasts as long as the clip does on the timeline at `rate`.
pub fn render(clip: &AudioClip, audio: &AudioData, tempo: &TempoMap, rate: u32) -> AudioData {
    let sr = f64::from(rate);
    let start = tempo.tick_to_seconds(clip.start.0 as f64);
    let end = tempo.tick_to_seconds((clip.start.0 + clip.length.0) as f64);
    let out_len = ((end - start) * sr).round().max(0.0) as usize;
    let file_rate = f64::from(audio.sample_rate);
    let offset = clip.source_offset.0 as f64;
    let source_at = |n: usize| {
        let tick = tempo.seconds_to_tick(start + n as f64 / sr);
        offset + clip.source_secs(tempo, tick) * file_rate
    };
    AudioData {
        sample_rate: rate,
        channels: musiton_dsp::stretch::wsola(&audio.channels, out_len, &source_at),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use musiton_core::ids::{AssetId, ClipId};
    use musiton_core::{SamplePos, Tick, PPQ};
    use musiton_project::Warp;

    /// A click every beat at 100 BPM, 48 kHz.
    fn clicks_at_100_bpm(beats: usize) -> AudioData {
        let beat = 48_000 * 60 / 100;
        let mut x = vec![0.0_f32; beats * beat];
        for b in 0..beats {
            for i in 0..200 {
                x[b * beat + i] = (std::f32::consts::TAU * i as f32 / 50.0).sin() * 0.8;
            }
        }
        AudioData {
            sample_rate: 48_000,
            channels: vec![x],
        }
    }

    fn onsets(x: &[f32]) -> Vec<usize> {
        let mut out = Vec::new();
        let mut quiet = 10_000;
        for (i, s) in x.iter().enumerate() {
            if s.abs() > 0.2 {
                if quiet > 4_000 {
                    out.push(i);
                }
                quiet = 0;
            } else {
                quiet += 1;
            }
        }
        out
    }

    #[test]
    fn a_warped_loop_lands_on_the_song_beats() {
        let audio = clicks_at_100_bpm(8);
        let clip = AudioClip {
            id: ClipId(1),
            asset: AssetId(1),
            start: Tick(0),
            length: Tick(8 * PPQ),
            source_offset: SamplePos(0),
            warp: Some(Warp { bpm: 100.0 }),
            ..AudioClip::default()
        };
        // Song at 120 BPM: 8 beats in 4 s, a click every 0.5 s.
        let tempo = TempoMap::constant(120.0);
        let out = render(&clip, &audio, &tempo, 48_000);
        assert_eq!(out.channels[0].len(), 192_000);
        let got = onsets(&out.channels[0]);
        assert_eq!(got.len(), 8, "{got:?}");
        for (k, at) in got.iter().enumerate() {
            let want = k * 24_000;
            assert!(at.abs_diff(want) < 600, "beat {k} at {at}, expected {want}");
        }
        // Another tempo map gives another rendering.
        assert_ne!(
            key(&clip, &tempo, 48_000),
            key(&clip, &TempoMap::constant(90.0), 48_000)
        );
        assert_eq!(key(&clip, &tempo, 48_000), key(&clip, &tempo, 48_000));
    }

    #[test]
    fn export_renders_warped_clips_in_time() {
        use musiton_project::{Track, TrackKind};
        use std::sync::atomic::AtomicBool;
        let clip = AudioClip {
            id: ClipId(1),
            asset: AssetId(1),
            start: Tick(0),
            length: Tick(8 * PPQ),
            source_offset: SamplePos(0),
            warp: Some(Warp { bpm: 100.0 }),
            ..AudioClip::default()
        };
        let mut project = Project {
            tempo: TempoMap::constant(120.0),
            ..Project::default()
        };
        project.tracks.push(Track {
            id: musiton_core::ids::TrackId(1),
            kind: TrackKind::Audio,
            clips: vec![Clip::Audio(clip)],
            ..Track::default()
        });
        let mut assets = AssetStore::default();
        assets.insert(AssetId(1), clicks_at_100_bpm(8));
        // Nothing rendered yet: the live graph keeps the clip silent…
        assert_eq!(todo(&project, &assets, 48_000).1.len(), 1);
        // …but an export renders it on the spot.
        let out = crate::export::render_project(
            &project,
            &assets,
            0,
            192_000,
            &|_| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        let got = onsets(&out[0]);
        assert_eq!(got.len(), 8, "{got:?}");
        for (k, at) in got.iter().enumerate() {
            assert!(at.abs_diff(k * 24_000) < 600, "beat {k} at {at}");
        }
    }
}
