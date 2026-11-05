//! Offline export: renders the project faster than real time on a worker thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};

use musiton_core::{SamplePos, Tick};
use musiton_engine::{create, render_offline, EngineCommand};
use musiton_io::export::{write_export, ExportOptions};
use musiton_project::{Clip, Project, TrackKind};

use crate::assets::AssetStore;
use crate::engine_graph;

/// Engine block size used for exports.
const BLOCK: usize = 512;
/// Frames rendered (stopped) before playback starts, so the graph has faded in.
const PRE_ROLL: usize = 1024;
/// Frames rendered per progress update.
const CHUNK: usize = 8192;

/// Which part of the song is exported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExportRange {
    /// From the start to the end of the last clip.
    #[default]
    Song,
    /// The loop range.
    Loop,
    /// From the first to the last selected clip.
    Selection,
}

/// Everything an export needs.
#[derive(Debug, Clone)]
pub struct ExportJob {
    /// Range start and end.
    pub range: (Tick, Tick),
    /// Extra seconds rendered after the end (reverb/delay tails).
    pub tail_secs: f32,
    /// File format etc.
    pub options: ExportOptions,
    /// One file per track instead of the mix.
    pub stems: bool,
    /// Output file (mix) or folder (stems).
    pub path: PathBuf,
}

/// End of the last clip (at least one bar).
pub fn song_end(project: &Project) -> Tick {
    let end = project
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter().map(Clip::end))
        .max()
        .unwrap_or(Tick(0));
    Tick(end.0.max(project.time_sig.ticks_per_bar()))
}

/// Renders `frames` frames of `project` starting at sample `start` (planar stereo).
/// `progress` receives 0..=1000 per mille of this render; `cancel` stops early (`None`).
pub fn render_project(
    project: &Project,
    assets: &AssetStore,
    start: u64,
    frames: usize,
    progress: &dyn Fn(f32),
    cancel: &AtomicBool,
) -> Option<Vec<Vec<f32>>> {
    let mut project = project.clone();
    project.metronome = false;
    let sr = project.sample_rate as f32;
    let built = engine_graph::build(&project, assets, sr, BLOCK, 0).ok()?;
    let params = built.info.params(&project, None);
    let automation = built.info.automation(&project, &[]);
    let (mut h, mut e) = create(sr, BLOCK);
    h.send(EngineCommand::SwapGraph(Box::new(built.graph)))
        .ok()?;
    for ((node, id), val) in params {
        h.send(EngineCommand::SetParam { node, id, val }).ok()?;
    }
    if !automation.0.is_empty() {
        h.send(EngineCommand::SetAutomation(Some(Box::new(automation))))
            .ok()?;
    }
    h.send(EngineCommand::SetTempoMap(Box::new(project.tempo.clone())))
        .ok()?;
    h.send(EngineCommand::SetTimeSig(project.time_sig)).ok()?;
    h.send(EngineCommand::Locate(SamplePos(start))).ok()?;
    render_offline(&mut e, PRE_ROLL, 2, BLOCK);
    h.send(EngineCommand::Play).ok()?;
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    let mut done = 0;
    while done < frames {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let n = CHUNK.min(frames - done);
        let out = render_offline(&mut e, n, 2, BLOCK);
        for f in out.chunks_exact(2) {
            left.push(f[0]);
            right.push(f[1]);
        }
        done += n;
        progress(done as f32 / frames as f32);
    }
    Some(vec![left, right])
}

/// A running export.
pub struct ExportTask {
    /// Progress 0..=1000.
    pub progress: Arc<AtomicU32>,
    /// Set to stop the export.
    pub cancel: Arc<AtomicBool>,
    rx: mpsc::Receiver<Result<Vec<PathBuf>, String>>,
}

impl ExportTask {
    /// The result once finished: written files, or an error / "cancelled".
    pub fn result(&self) -> Option<Result<Vec<PathBuf>, String>> {
        self.rx.try_recv().ok()
    }
}

/// File name for the stem of `track` next to `dir`.
fn stem_path(dir: &Path, song: &str, track: &str, ext: &str) -> PathBuf {
    let clean: String = track
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.join(format!("{song} - {clean}.{ext}"))
}

/// Starts an export on a worker thread.
pub fn start(
    job: ExportJob,
    project: Project,
    assets: AssetStore,
    song_name: String,
) -> ExportTask {
    let progress = Arc::new(AtomicU32::new(0));
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let (p, c) = (progress.clone(), cancel.clone());
    let spawned = std::thread::Builder::new()
        .name("musiton-export".into())
        .spawn(move || {
            let _ = tx.send(run(&job, &project, &assets, &song_name, &p, &c));
        });
    if let Err(e) = spawned {
        tracing::error!("cannot start export thread: {e}");
    }
    ExportTask {
        progress,
        cancel,
        rx,
    }
}

fn run(
    job: &ExportJob,
    project: &Project,
    assets: &AssetStore,
    song: &str,
    progress: &AtomicU32,
    cancel: &AtomicBool,
) -> Result<Vec<PathBuf>, String> {
    let sr = f64::from(project.sample_rate);
    let at = |t: Tick| {
        project
            .tempo
            .tick_to_sample(t.0 as f64, sr)
            .round()
            .max(0.0) as u64
    };
    let start = at(job.range.0);
    let end = at(job.range.1) + (f64::from(job.tail_secs.max(0.0)) * sr) as u64;
    let frames = end.saturating_sub(start) as usize;
    if frames == 0 {
        return Err("The export range is empty.".into());
    }
    // (file, project to render): the mix, or one soloed copy per track.
    let mut renders: Vec<(PathBuf, Project)> = Vec::new();
    if job.stems {
        std::fs::create_dir_all(&job.path).map_err(|e| e.to_string())?;
        for t in project
            .tracks
            .iter()
            .filter(|t| t.kind != TrackKind::Folder)
        {
            let mut solo = project.clone();
            for x in &mut solo.tracks {
                x.strip.solo = x.id == t.id;
            }
            renders.push((
                stem_path(&job.path, song, &t.name, job.options.format.extension()),
                solo,
            ));
        }
    } else {
        renders.push((job.path.clone(), project.clone()));
    }
    let total = renders.len().max(1) as f32;
    let mut written = Vec::new();
    for (k, (path, p)) in renders.iter().enumerate() {
        let report =
            |x: f32| progress.store(((k as f32 + x) / total * 1000.0) as u32, Ordering::Relaxed);
        let audio =
            render_project(p, assets, start, frames, &report, cancel).ok_or("Export cancelled.")?;
        write_export(path, &audio, project.sample_rate, job.options).map_err(|e| e.to_string())?;
        written.push(path.clone());
    }
    progress.store(1000, Ordering::Relaxed);
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use musiton_io::export::ExportFormat;

    /// The filter-sweep project of the automation acceptance test.
    fn sweep_project() -> Project {
        use musiton_core::ids::{ClipId, DeviceId, TrackId};
        use musiton_core::{TempoMap, PPQ};
        use musiton_devices::polysynth::id;
        use musiton_project::{
            AutoMode, AutoPoint, AutomationLane, DeviceSlot, MidiClip, Note, ParamTarget, Track,
        };
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
        project
    }

    /// Phase 13 acceptance: a 32-bit float export (no dither, no normalize) is
    /// bit-identical to the offline golden render of the same project.
    #[test]
    fn float_export_is_bit_identical_to_the_golden_render() {
        let project = sweep_project();
        let dir = std::env::temp_dir().join(format!("musiton-export-app-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let job = ExportJob {
            range: (Tick(0), Tick(4 * musiton_core::PPQ)),
            tail_secs: 0.0,
            options: ExportOptions {
                format: ExportFormat::Wav32Float,
                normalize: false,
                dither: false,
            },
            stems: false,
            path: dir.join("mix.wav"),
        };
        let progress = AtomicU32::new(0);
        let files = run(
            &job,
            &project,
            &AssetStore::default(),
            "Song",
            &progress,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(progress.load(Ordering::Relaxed), 1000);
        let exported = musiton_io::file::decode_file(&files[0]).unwrap();
        let golden_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden/filter_sweep.wav");
        let golden = musiton_io::file::decode_file(&golden_path).unwrap();
        assert_eq!(exported.frames(), golden.frames());
        assert!(
            exported.channel(0) == golden.channel(0),
            "export differs from the golden render"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stems_solo_each_track_and_cancel_stops() {
        let mut project = sweep_project();
        let mut second = project.tracks[0].clone();
        second.id = musiton_core::ids::TrackId(2);
        second.name = "Lead/2".into();
        project.tracks.push(second);
        let dir = std::env::temp_dir().join(format!("musiton-stems-{}", std::process::id()));
        let job = ExportJob {
            range: (Tick(0), Tick(musiton_core::PPQ)),
            tail_secs: 0.1,
            options: ExportOptions {
                format: ExportFormat::Flac24,
                normalize: true,
                dither: false,
            },
            stems: true,
            path: dir.clone(),
        };
        let files = run(
            &job,
            &project,
            &AssetStore::default(),
            "Song",
            &AtomicU32::new(0),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(files.len(), 2);
        assert!(files[1].ends_with("Song - Lead_2.flac"));
        let a = musiton_io::file::decode_file(&files[0]).unwrap();
        assert_eq!(a.frames(), 24_000 + 4_800);
        assert!(run(
            &job,
            &project,
            &AssetStore::default(),
            "Song",
            &AtomicU32::new(0),
            &AtomicBool::new(true)
        )
        .is_err());
        assert_eq!(song_end(&project), Tick(8 * musiton_core::PPQ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
