//! Decoded audio of the open project, waveform peaks and background importing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;

use musiton_core::audio::{AudioData, Peaks};
use musiton_core::ids::{AssetId, TrackId};
use musiton_core::{TempoMap, Tick};
use musiton_devices::sfz_player::{Looping, SfzRegion, SfzSetup};
use musiton_io::file::{import_audio, Imported};
use musiton_io::sfz::{LoopMode, Trigger};
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
    /// Time-stretched renderings of warped clips, by [`crate::warp::key`].
    warped: HashMap<u64, Arc<AudioData>>,
    /// Sample instruments by SFZ path (regions with their decoded samples).
    sfz: HashMap<String, Arc<LoadedSfz>>,
    /// Bumped whenever the set of loaded files changes (triggers a graph rebuild).
    pub generation: u64,
}

impl AssetStore {
    /// Loads every asset of `project` from `dir`. Returns the assets that failed.
    pub fn load_project(
        &mut self,
        project: &Project,
        dir: Option<&Path>,
    ) -> Vec<(AssetId, String)> {
        *self = Self {
            generation: self.generation + 1,
            ..Self::default()
        };
        let mut failed = Vec::new();
        for asset in &project.assets {
            let Some(dir) = dir else {
                failed.push((asset.id, asset.file.clone()));
                continue;
            };
            match musiton_io::file::decode_file(&dir.join(&asset.file)) {
                Ok(audio) => self.insert(asset.id, audio),
                Err(e) => {
                    tracing::error!("cannot load {}: {e}", asset.file);
                    failed.push((asset.id, asset.file.clone()));
                }
            }
        }
        failed
    }

    /// SFZ instruments the project uses that are not loaded yet (each path once).
    pub fn missing_sfz(&self, project: &Project) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for path in project
            .tracks
            .iter()
            .filter_map(|t| t.devices.first()?.sfz())
        {
            if !self.sfz.contains_key(path) && !out.iter().any(|p| p == path) {
                out.push(path.to_owned());
            }
        }
        out
    }

    /// Adds a loaded SFZ instrument (the graph is rebuilt with it).
    pub fn insert_sfz(&mut self, path: String, loaded: LoadedSfz) {
        self.sfz.insert(path, Arc::new(loaded));
        self.generation += 1;
    }

    /// A loaded SFZ instrument.
    pub fn sfz(&self, path: &str) -> Option<&Arc<LoadedSfz>> {
        self.sfz.get(path)
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

    /// The rendering of a warped clip, if it is ready.
    pub fn warped(&self, key: u64) -> Option<&Arc<AudioData>> {
        self.warped.get(&key)
    }

    /// Stores the rendering of a warped clip (triggers a graph rebuild).
    pub fn insert_warped(&mut self, key: u64, audio: AudioData) {
        self.warped.insert(key, Arc::new(audio));
        self.generation += 1;
    }

    /// Drops renderings that are no longer in `keys`.
    pub fn keep_warped(&mut self, keys: &std::collections::HashSet<u64>) {
        self.warped.retain(|k, _| keys.contains(k));
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

/// A finished background SFZ load: the path and the instrument or an error.
pub type SfzDone = (String, Result<LoadedSfz, String>);

/// Reads SFZ instruments on worker threads (large libraries take seconds to decode).
pub struct SfzLoader {
    tx: mpsc::Sender<SfzDone>,
    rx: mpsc::Receiver<SfzDone>,
    pending: Vec<String>,
}

impl Default for SfzLoader {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            tx,
            rx,
            pending: Vec::new(),
        }
    }
}

impl SfzLoader {
    /// Starts loading `path` unless it is already loading.
    pub fn start(&mut self, path: String) {
        if self.pending.contains(&path) {
            return;
        }
        let tx = self.tx.clone();
        let p = path.clone();
        let spawned = std::thread::Builder::new()
            .name("musiton-sfz".into())
            .spawn(move || {
                let result = load_sfz_file(Path::new(&p));
                let _ = tx.send((p, result));
            });
        match spawned {
            Ok(_) => self.pending.push(path),
            Err(e) => tracing::error!("cannot start SFZ loading thread: {e}"),
        }
    }

    /// Loads that finished since the last call.
    pub fn finished(&mut self) -> Vec<SfzDone> {
        let done: Vec<SfzDone> = self.rx.try_iter().collect();
        self.pending.retain(|p| done.iter().all(|(d, _)| d != p));
        done
    }

    /// Whether any instrument is still loading.
    pub fn busy(&self) -> bool {
        !self.pending.is_empty()
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

/// An SFZ instrument ready to play: regions with decoded samples and its settings.
#[derive(Debug, Clone, Default)]
pub struct LoadedSfz {
    /// The playable regions.
    pub regions: Vec<SfzRegion>,
    /// Key switches and controller start values.
    pub setup: SfzSetup,
}

/// Reads `path` and builds playable regions; every sample file is decoded once. Regions
/// whose sample cannot be read are left out (logged).
pub fn load_sfz_file(path: &Path) -> Result<LoadedSfz, String> {
    let inst = musiton_io::sfz::read_sfz(path).map_err(|e| e.to_string())?;
    let setup = SfzSetup {
        keyswitch_range: inst.sw_range,
        keyswitch_default: inst.sw_default,
        cc_defaults: inst.cc_defaults.clone(),
    };
    let mut audio: HashMap<PathBuf, Option<Arc<AudioData>>> = HashMap::new();
    let mut out = Vec::with_capacity(inst.regions.len());
    for r in inst.regions {
        let sample = audio
            .entry(r.sample.clone())
            .or_insert_with(|| match musiton_io::file::decode_file(&r.sample) {
                Ok(a) => Some(Arc::new(a)),
                Err(e) => {
                    tracing::warn!("SFZ sample {} unreadable: {e}", r.sample.display());
                    None
                }
            })
            .clone();
        let Some(sample) = sample else { continue };
        out.push(SfzRegion {
            keys: (r.lokey, r.hikey),
            vels: (r.lovel.max(1), r.hivel),
            keycenter: r.pitch_keycenter,
            keytrack: r.pitch_keytrack,
            tune: r.tune,
            volume_db: r.volume,
            pan: r.pan,
            veltrack: r.amp_veltrack,
            attack: r.ampeg_attack,
            decay: r.ampeg_decay,
            sustain: r.ampeg_sustain / 100.0,
            release: r.ampeg_release,
            offset: r.offset,
            on_release: r.trigger == Trigger::Release,
            looping: match r.loop_mode {
                LoopMode::NoLoop => Looping::None,
                LoopMode::OneShot => Looping::OneShot,
                LoopMode::Continuous => Looping::Continuous,
                LoopMode::Sustain => Looping::Sustain,
            },
            loop_points: r.loop_start.zip(r.loop_end),
            seq_length: r.seq_length,
            seq_position: r.seq_position,
            rand: (r.lorand, r.hirand),
            group: r.group,
            off_by: r.off_by,
            keyswitch: r.sw_last,
            cc_ranges: r.cc_ranges,
            ..SfzRegion::new(sample)
        });
    }
    if out.is_empty() {
        return Err("no playable regions (are the sample files next to it?)".into());
    }
    Ok(LoadedSfz {
        regions: out,
        setup,
    })
}

#[cfg(test)]
mod sfz_tests {
    use super::*;
    use musiton_io::export::{write_export, ExportFormat, ExportOptions};

    #[test]
    fn an_sfz_instrument_loads_and_plays_in_a_song() {
        let dir = std::env::temp_dir().join(format!("musiton-sfz-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("samples")).unwrap();
        let tone: Vec<f32> = (0..24_000)
            .map(|i| (std::f32::consts::TAU * 440.0 * i as f32 / 48_000.0).sin() * 0.5)
            .collect();
        let opts = ExportOptions {
            format: ExportFormat::Wav24,
            normalize: false,
            dither: false,
        };
        write_export(&dir.join("samples/a4.wav"), &[tone], 48_000, opts).unwrap();
        let sfz = dir.join("test.sfz");
        std::fs::write(
            &sfz,
            "<control> default_path=samples/\n<region> sample=a4.wav pitch_keycenter=69 ampeg_release=0.1\n<region> sample=missing.wav lokey=0 hikey=10\n",
        )
        .unwrap();
        let path = sfz.to_string_lossy().into_owned();
        let mut assets = AssetStore::default();
        let loaded = load_sfz_file(Path::new(&path)).unwrap();
        assert_eq!(loaded.regions.len(), 1, "the missing sample is skipped");
        assets.insert_sfz(path.clone(), loaded);

        let mut project = crate::templates::assemble(None, 120.0, 1, 1, || {
            vec![crate::templates::Part {
                name: "Piano",
                color: [0, 0, 0],
                trim_db: 0.0,
                instrument: musiton_devices::SFZ,
                params: Vec::new(),
                effects: Vec::new(),
                notes: vec![crate::templates::note(69, 100, 0, 480)],
            }]
        });
        if let Some(musiton_project::DeviceSlot {
            kind: musiton_project::DeviceKind::Builtin { sfz, .. },
            ..
        }) = project.tracks[0].devices.first_mut()
        {
            *sfz = Some(path.clone());
        }
        assert!(assets.missing_sfz(&project).is_empty());
        assert_eq!(
            AssetStore::default().missing_sfz(&project),
            vec![path.clone()]
        );
        let out = crate::export::render_project(
            &project,
            &assets,
            &crate::engine_graph::PluginNodes::default(),
            0,
            24_000,
            &|_| {},
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
        let peak = out[0].iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.05, "peak {peak}");

        // In the background: the loader reports the same instrument, once per path.
        let mut loader = SfzLoader::default();
        loader.start(path.clone());
        loader.start(path.clone());
        assert!(loader.busy());
        let mut done = Vec::new();
        for _ in 0..500 {
            done.extend(loader.finished());
            if !loader.busy() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!loader.busy());
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].0, path);
        assert_eq!(done[0].1.as_ref().map(|l| l.regions.len()), Ok(1));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
