//! The built-in demo song (generated, so it needs no audio files) and a larger
//! variant used to measure performance.

use std::path::{Path, PathBuf};

use musiton_devices::effects::{compressor, delay, reverb};
use musiton_devices::polysynth::id;
use musiton_project::{Note, Project, ProjectError, EXTENSION};

use crate::templates::{assemble, note, preset, Part, BAR, STEP};

/// Length of the demo loop in bars (4/4).
const BARS: i64 = 8;

/// Notes on every bar for each step in `steps` (16 steps per bar).
fn pattern(pitch: u8, velocity: u8, steps: &[i64], length: i64) -> Vec<Note> {
    (0..BARS)
        .flat_map(|bar| {
            steps
                .iter()
                .map(move |s| note(pitch, velocity, bar * BAR + s * STEP, length))
        })
        .collect()
}

/// Root notes of the chord progression Am – F – C – G (one bar each).
const ROOTS: [u8; 4] = [57, 53, 48, 55];
/// Chord tones relative to the root (minor for Am, major otherwise).
fn chord(bar: usize) -> [u8; 3] {
    let root = ROOTS[bar % 4];
    let third = if bar.is_multiple_of(4) { 3 } else { 4 };
    [root, root + third, root + 7]
}

fn parts() -> Vec<Part> {
    let kick = pattern(36, 120, &[0, 4, 8, 12], STEP);
    let snare = pattern(50, 100, &[4, 12], STEP);
    let hats = pattern(42, 70, &[0, 2, 4, 6, 8, 10, 12, 14], STEP / 2);
    let bass: Vec<Note> = (0..BARS as usize)
        .flat_map(|bar| {
            let root = ROOTS[bar % 4] - 24;
            [0, 3, 6, 10, 12, 14].into_iter().map(move |s| {
                let pitch = if s == 10 { root + 12 } else { root };
                note(pitch, 105, bar as i64 * BAR + s * STEP, 2 * STEP - 20)
            })
        })
        .collect();
    let pad: Vec<Note> = (0..BARS as usize)
        .flat_map(|bar| chord(bar).map(|p| note(p, 75, bar as i64 * BAR, BAR - 10)))
        .collect();
    let lead: Vec<Note> = (0..BARS as usize)
        .flat_map(|bar| {
            let c = chord(bar);
            let line = [c[2] + 12, c[1] + 12, c[0] + 12, c[1] + 12];
            [0, 3, 6, 10]
                .into_iter()
                .zip(line)
                .map(move |(s, p)| note(p, 90, bar as i64 * BAR + s * STEP, STEP))
        })
        .collect();
    vec![
        Part {
            name: "Kick",
            color: [220, 90, 80],
            trim_db: 11.0,
            instrument: musiton_devices::POLYSYNTH,
            params: vec![
                (id::OSC1_WAVE, 3.0),
                (id::OSC1_LEVEL, 1.0),
                (id::CUTOFF, 300.0),
                (id::FILTER_ENV, 0.0),
                (id::AMP_D, 0.25),
                (id::AMP_S, 0.0),
                (id::AMP_R, 0.05),
            ],
            effects: vec![("compressor", vec![(compressor::id::THRESHOLD, -12.0)])],
            notes: kick,
        },
        Part {
            name: "Snare",
            color: [230, 160, 70],
            trim_db: 15.0,
            instrument: musiton_devices::POLYSYNTH,
            params: vec![
                (id::OSC1_WAVE, 2.0),
                (id::OSC1_LEVEL, 0.5),
                (id::NOISE, 1.0),
                (id::CUTOFF, 7_000.0),
                (id::FILTER_ENV, 0.0),
                (id::AMP_D, 0.15),
                (id::AMP_S, 0.0),
                (id::AMP_R, 0.08),
            ],
            effects: vec![("reverb", vec![(reverb::id::MIX, 0.2)])],
            notes: snare,
        },
        Part {
            name: "Hi-hat",
            color: [230, 210, 90],
            trim_db: 12.0,
            instrument: musiton_devices::POLYSYNTH,
            params: vec![
                (id::OSC1_LEVEL, 0.0),
                (id::NOISE, 1.0),
                (id::FILTER_MODE, 1.0),
                (id::CUTOFF, 8_000.0),
                (id::FILTER_ENV, 0.0),
                (id::AMP_D, 0.04),
                (id::AMP_S, 0.0),
                (id::AMP_R, 0.03),
            ],
            effects: Vec::new(),
            notes: hats,
        },
        Part {
            name: "Bass",
            color: [90, 170, 230],
            trim_db: 2.0,
            instrument: musiton_devices::POLYSYNTH,
            params: synth_preset("Bass"),
            effects: vec![("eq", Vec::new())],
            notes: bass,
        },
        Part {
            name: "Pad",
            color: [150, 110, 220],
            trim_db: -5.0,
            instrument: musiton_devices::POLYSYNTH,
            params: synth_preset("Warm pad"),
            effects: vec![("reverb", vec![(reverb::id::MIX, 0.35)])],
            notes: pad,
        },
        Part {
            name: "Lead",
            color: [100, 200, 140],
            trim_db: 7.0,
            instrument: musiton_devices::POLYSYNTH,
            params: synth_preset("Pluck"),
            effects: vec![("delay", vec![(delay::id::MIX, 0.25)])],
            notes: lead,
        },
    ]
}

fn synth_preset(name: &str) -> Vec<(u64, f32)> {
    preset(musiton_devices::POLYSYNTH, name)
}

/// Builds a demo project with `copies` copies of each part (1 = the normal demo).
fn build(copies: usize) -> Project {
    assemble(None, 112.0, BARS, copies, parts)
}

/// The demo song.
pub fn demo_project() -> Project {
    build(1)
}

/// A song with at least `tracks` instrument tracks, each with synth + effects.
#[cfg(test)]
fn stress_project(tracks: usize) -> Project {
    build(tracks.div_ceil(6))
}

/// Writes the demo song to `<parent>/Musiton Demo/Musiton Demo.musiton` (if not there yet)
/// and returns its path.
pub fn install(parent: &Path) -> Result<PathBuf, ProjectError> {
    let dir = parent.join("Musiton Demo");
    let path = dir.join(format!("Musiton Demo.{EXTENSION}"));
    if !path.exists() {
        std::fs::create_dir_all(&dir)?;
        musiton_project::write_file(&demo_project(), &path)?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::AssetStore;
    use std::sync::atomic::AtomicBool;
    use std::time::Instant;

    #[test]
    fn demo_plays_audio_without_clipping() {
        let project = demo_project();
        assert_eq!(project.tracks.len(), 6);
        let frames = 48_000 * 4;
        let out = crate::export::render_project(
            &project,
            &AssetStore::default(),
            &crate::engine_graph::PluginNodes::default(),
            0,
            frames,
            &|_| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        let peak = out.iter().flatten().fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.05 && peak < 1.0, "peak {peak}");
    }

    #[test]
    fn install_writes_a_loadable_project() {
        let dir = std::env::temp_dir().join(format!("musiton-demo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = install(&dir).unwrap();
        assert_eq!(musiton_project::load(&path).unwrap(), demo_project());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Performance check: `cargo test --release -p musiton-app perf -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing measurement, run in release mode"]
    fn perf_32_tracks() {
        let project = stress_project(32);
        let secs = std::env::var("MUSITON_PERF_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10);
        let t = Instant::now();
        crate::export::render_project(
            &project,
            &AssetStore::default(),
            &crate::engine_graph::PluginNodes::default(),
            0,
            48_000 * secs,
            &|_| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        let used = t.elapsed().as_secs_f64() / secs as f64;
        println!(
            "{} tracks: {:.1} % of one core for real-time playback",
            project.tracks.len(),
            used * 100.0
        );
    }
}
