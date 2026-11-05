//! Decoded audio of the open project, waveform peaks and background importing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;

use musiton_core::audio::{AudioData, Peaks};
use musiton_core::ids::{AssetId, TrackId};
use musiton_core::{TempoMap, Tick};
use musiton_io::file::{import_audio, Imported};
use musiton_project::Project;

/// Result of a background rate conversion: per asset the converted audio (`None` = failed).
pub type Converted = Vec<(AssetId, Option<AudioData>)>;

/// Audio files of the project in memory (cloning shares the audio).
#[derive(Default, Clone)]
pub struct AssetStore {
    audio: HashMap<AssetId, Arc<AudioData>>,
    /// Copies converted to the audio device's rate (when it differs from the project's).
    playback: HashMap<AssetId, Arc<AudioData>>,
    peaks: HashMap<AssetId, Arc<Peaks>>,
    /// Bumped whenever the set of loaded files changes (triggers a graph rebuild).
    pub generation: u64,
}

impl AssetStore {
    /// Loads every asset of `project` from `dir`. Returns the files that failed.
    pub fn load_project(&mut self, project: &Project, dir: Option<&Path>) -> Vec<String> {
        *self = Self {
            generation: self.generation + 1,
            ..Self::default()
        };
        let mut failed = Vec::new();
        for asset in &project.assets {
            let Some(dir) = dir else {
                failed.push(asset.file.clone());
                continue;
            };
            match musiton_io::file::decode_file(&dir.join(&asset.file)) {
                Ok(audio) => self.insert(asset.id, audio),
                Err(e) => {
                    tracing::error!("cannot load {}: {e}", asset.file);
                    failed.push(asset.file.clone());
                }
            }
        }
        failed
    }

    /// Adds decoded audio (computes its peaks).
    pub fn insert(&mut self, id: AssetId, audio: AudioData) {
        self.peaks.insert(id, Arc::new(Peaks::compute(&audio)));
        self.audio.insert(id, Arc::new(audio));
        self.generation += 1;
    }

    /// Audio of an asset for playback at `rate`: the converted copy if there is one,
    /// otherwise the original (the clip player then interpolates).
    pub fn audio_at(&self, id: AssetId, rate: u32) -> Option<&Arc<AudioData>> {
        self.playback
            .get(&id)
            .filter(|a| a.sample_rate == rate)
            .or_else(|| self.audio.get(&id))
    }

    /// Assets that are not at `rate` and have no converted copy yet.
    pub fn needs_conversion(&self, rate: u32) -> Vec<(AssetId, Arc<AudioData>)> {
        self.audio
            .iter()
            .filter(|(id, a)| {
                a.sample_rate != rate && self.playback.get(id).is_none_or(|p| p.sample_rate != rate)
            })
            .map(|(id, a)| (*id, a.clone()))
            .collect()
    }

    /// Stores a copy converted for playback (triggers a graph rebuild).
    pub fn insert_playback(&mut self, id: AssetId, audio: AudioData) {
        if self.audio.contains_key(&id) {
            self.playback.insert(id, Arc::new(audio));
            self.generation += 1;
        }
    }

    /// Waveform peaks of all loaded assets.
    pub fn peaks(&self) -> &HashMap<AssetId, Arc<Peaks>> {
        &self.peaks
    }
}

/// Where imported audio should go.
#[derive(Debug, Clone, Copy)]
pub struct ImportTarget {
    /// Track to put the clip on; `None` creates a new audio track.
    pub track: Option<TrackId>,
    /// Start position of the clip.
    pub at: Tick,
}

/// A finished background import.
pub struct ImportDone {
    /// The source file.
    pub source: PathBuf,
    /// Where the clip goes.
    pub target: ImportTarget,
    /// The result.
    pub result: Result<Imported, String>,
}

/// Runs imports on worker threads so the UI never blocks.
pub struct Importer {
    tx: mpsc::Sender<ImportDone>,
    rx: mpsc::Receiver<ImportDone>,
    running: usize,
}

impl Default for Importer {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self { tx, rx, running: 0 }
    }
}

impl Importer {
    /// Starts importing `files` into `project_dir`. Several files go on consecutive tracks
    /// when no track is given, or one after another on the same track otherwise.
    pub fn start(
        &mut self,
        source: PathBuf,
        project_dir: PathBuf,
        rate: u32,
        target: ImportTarget,
    ) {
        let tx = self.tx.clone();
        self.running += 1;
        let spawned = std::thread::Builder::new()
            .name("musiton-import".into())
            .spawn(move || {
                let result = import_audio(&source, &project_dir, rate).map_err(|e| e.to_string());
                let _ = tx.send(ImportDone {
                    source,
                    target,
                    result,
                });
            });
        if let Err(e) = spawned {
            self.running -= 1;
            tracing::error!("cannot start import thread: {e}");
        }
    }

    /// Imports that finished since the last call.
    pub fn finished(&mut self) -> Vec<ImportDone> {
        let done: Vec<ImportDone> = self.rx.try_iter().collect();
        self.running -= done.len().min(self.running);
        done
    }

    /// Number of imports still running.
    pub fn running(&self) -> usize {
        self.running
    }
}

/// Where a recorded take goes on the timeline: (start, end, frames to skip at the file
/// start). The take was captured at song sample `start`; it is moved `latency` frames
/// earlier. `None` if nothing remains after compensation.
pub fn place_take(
    tempo: &TempoMap,
    start: u64,
    frames: usize,
    latency: i64,
    sample_rate: f64,
) -> Option<(Tick, Tick, u64)> {
    let first = start as i64 - latency;
    let skip = (-first).max(0) as u64;
    let last = first + frames as i64;
    if last <= 0 {
        return None;
    }
    let at = |s: i64| Tick(tempo.sample_to_tick(s.max(0) as f64, sample_rate).round() as i64);
    let (a, b) = (at(first), at(last));
    (b.0 > a.0).then_some((a, b, skip))
}

#[cfg(test)]
mod tests {
    use super::*;
    use musiton_core::PPQ;

    #[test]
    fn playback_copies_are_used_only_at_their_rate() {
        let mut store = AssetStore::default();
        let id = AssetId(1);
        let tone = |rate: u32| AudioData {
            sample_rate: rate,
            channels: vec![vec![0.25; rate as usize / 10]],
        };
        store.insert(id, tone(48_000));
        assert!(store.needs_conversion(48_000).is_empty());
        assert_eq!(store.needs_conversion(44_100).len(), 1);
        let gen = store.generation;
        let converted = musiton_io::file::resample(tone(48_000), 44_100).unwrap();
        store.insert_playback(id, converted);
        assert!(store.generation > gen);
        assert!(store.needs_conversion(44_100).is_empty());
        assert_eq!(store.audio_at(id, 44_100).unwrap().sample_rate, 44_100);
        // Export at the project rate still gets the original.
        assert_eq!(store.audio_at(id, 48_000).unwrap().sample_rate, 48_000);
        // Another device rate needs a new conversion.
        assert_eq!(store.needs_conversion(96_000).len(), 1);
    }

    #[test]
    fn takes_move_earlier_by_the_latency() {
        let tempo = TempoMap::constant(120.0);
        // Captured at beat 2 (24 000) plus 480 frames latency → placed exactly on beat 2.
        let (a, b, skip) = place_take(&tempo, 24_480, 24_000, 480, 48_000.0).unwrap();
        assert_eq!((a, b, skip), (Tick(PPQ), Tick(2 * PPQ), 0));
        // A take that starts at 0 loses its first `latency` frames instead.
        let (a, _, skip) = place_take(&tempo, 0, 1000, 300, 48_000.0).unwrap();
        assert_eq!((a, skip), (Tick(0), 300));
        assert!(place_take(&tempo, 0, 100, 300, 48_000.0).is_none());
    }
}
