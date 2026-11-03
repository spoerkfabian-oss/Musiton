//! Decoded audio of the open project, waveform peaks and background importing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;

use musiton_core::audio::{AudioData, Peaks};
use musiton_core::ids::{AssetId, TrackId};
use musiton_core::Tick;
use musiton_io::file::{import_audio, Imported};
use musiton_project::Project;

/// Audio files of the project in memory.
#[derive(Default)]
pub struct AssetStore {
    audio: HashMap<AssetId, Arc<AudioData>>,
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

    /// Decoded audio of an asset.
    pub fn audio(&self, id: AssetId) -> Option<&Arc<AudioData>> {
        self.audio.get(&id)
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
