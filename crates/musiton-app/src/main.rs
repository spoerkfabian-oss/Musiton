//! Musiton — a digital audio workstation.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod assets;
mod audio_dialog;
mod auto_record;
mod browser;
mod demo;
mod document;
mod engine_graph;
mod export;
mod export_dialog;
mod midi_record;
mod presets;
mod session;
mod settings;
mod settings_dialog;
mod transport;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use musiton_engine::EngineHandle;
use musiton_io::audio::{self, AudioOutput, OutputRequest};
use musiton_project::EXTENSION;
use musiton_ui::arrange::{Arrange, ArrangeInput};
use musiton_ui::UiAction;
use tracing_subscriber::EnvFilter;

use assets::{AssetStore, ImportDone, ImportTarget, Importer};
use audio_dialog::AudioDialog;
use browser::{Browser, BrowserAction};
use document::Document;
use settings::{AudioSettings, Settings};
use transport::{EngineSync, Transport};

/// In debug builds every allocation inside `musiton_engine::rt::no_alloc` aborts.
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: musiton_engine::rt::AllocDisabler = musiton_engine::rt::AllocDisabler;

/// Application name shown in the window title.
const APP_NAME: &str = "Musiton";
/// How long status messages stay visible.
const STATUS_TIME: Duration = Duration::from_secs(5);

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,wgpu=warn")),
        )
        .init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting {APP_NAME}");

    let open_arg = std::env::args_os().nth(1).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size([1280.0, 800.0])
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png"))
                    .unwrap_or_default(),
            ),
        ..Default::default()
    };
    eframe::run_native(
        APP_NAME,
        options,
        Box::new(|_cc| Ok(Box::new(App::new(open_arg)))),
    )
    .map_err(|e| anyhow::anyhow!("failed to start UI: {e}"))
}

/// Something that has to wait until the user decided about unsaved changes.
#[derive(Debug, Clone, PartialEq)]
enum Pending {
    New,
    Open(Option<PathBuf>),
    Quit,
}

/// Top-level application state (UI thread).
struct App {
    settings: Settings,
    settings_path: Option<PathBuf>,
    doc: Document,
    output: Option<(AudioOutput, EngineHandle)>,
    sync: EngineSync,
    transport: Transport,
    audio_error: Option<String>,
    audio_dialog: AudioDialog,
    status: Option<(String, Instant)>,
    pending: Option<Pending>,
    allow_close: bool,
    title: String,
    arrange: Arrange,
    mixer: musiton_ui::mixer::Mixer,
    piano_roll: musiton_ui::piano_roll::PianoRoll,
    device_panel: musiton_ui::device_panel::DevicePanel,
    /// User presets per device type: (name, values).
    user_presets: presets::PresetMap,
    bottom_tab: BottomTab,
    show_shortcuts: bool,
    assets: AssetStore,
    importer: Importer,
    graph: GraphState,
    browser: Browser,
    input: Option<musiton_io::audio::AudioInputStream>,
    input_error: Option<String>,
    writers: Vec<musiton_io::recording::DiskWriter>,
    measuring: bool,
    midi: Option<musiton_io::midi::MidiInputStream>,
    midi_error: Option<String>,
    /// Live MIDI recorded per track, waiting for recording to end.
    midi_rec:
        std::collections::HashMap<musiton_core::ids::TrackId, Vec<musiton_engine::nodes::RecMidi>>,
    /// Control moves being recorded into automation lanes.
    auto_rec: auto_record::AutoRecorder,
    export_dialog: export_dialog::ExportDialog,
    settings_dialog: settings_dialog::SettingsDialog,
    /// Offered after a crash (one at a time).
    recovery: Vec<session::Recovery>,
    /// An error to show in a dialog.
    error: Option<String>,
    /// Set after an internal error: the UI only shows the error dialog.
    broken: bool,
    /// This session's crash-detection lock.
    session: Option<session::SessionLock>,
    export_task: Option<export::ExportTask>,
    /// Transport state of the previous frame.
    was_playing: bool,
}

/// Effect types for the device panel.
fn effect_types() -> Vec<musiton_ui::device_panel::DeviceType> {
    musiton_devices::EFFECTS
        .iter()
        .map(|t| musiton_ui::device_panel::DeviceType {
            type_id: t.type_id,
            name: t.name,
            params: t.params,
            uses_samples: false,
            presets: Vec::new(),
            drum_param: None,
            root_param: None,
        })
        .collect()
}

/// What the running engine graph was built from.
#[derive(Default)]
struct GraphState {
    revision: Option<u64>,
    asset_gen: u64,
    structure: Option<engine_graph::Structure>,
    info: engine_graph::GraphInfo,
    /// Parameter values already sent to the engine.
    sent: std::collections::HashMap<engine_graph::ParamKey, f32>,
    /// Recording chunks the disk could not keep up with.
    record_dropped: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Automation last sent to the engine.
    sent_automation: Option<musiton_engine::automation::Automation>,
    /// Notes last sent to each instrument track's node.
    sent_notes:
        std::collections::HashMap<musiton_core::ids::TrackId, Vec<musiton_engine::nodes::MidiNote>>,
    /// Document revision the notes were last checked at.
    notes_rev: Option<u64>,
    /// Live MIDI captured by armed instrument tracks.
    midi_taps: Vec<(
        musiton_core::ids::TrackId,
        rtrb::Consumer<musiton_engine::nodes::RecMidi>,
    )>,
}

/// Tabs of the bottom panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum BottomTab {
    #[default]
    Editor,
    Device,
    Mixer,
}

impl App {
    fn new(open: Option<PathBuf>) -> Self {
        let settings_path = Settings::default_path();
        let settings = settings_path
            .as_deref()
            .map(Settings::load_from)
            .unwrap_or_default();
        let mut app = Self {
            settings,
            settings_path,
            doc: Document::default(),
            output: None,
            sync: EngineSync::default(),
            transport: Transport::default(),
            audio_error: None,
            audio_dialog: AudioDialog::default(),
            status: None,
            pending: None,
            allow_close: false,
            title: String::new(),
            arrange: Arrange::new(),
            mixer: musiton_ui::mixer::Mixer::default(),
            piano_roll: musiton_ui::piano_roll::PianoRoll::default(),
            device_panel: musiton_ui::device_panel::DevicePanel::default(),
            user_presets: presets::load_all(),
            bottom_tab: BottomTab::default(),
            show_shortcuts: false,
            assets: AssetStore::default(),
            importer: Importer::default(),
            graph: GraphState::default(),
            browser: Browser::default(),
            input: None,
            input_error: None,
            writers: Vec::new(),
            measuring: false,
            midi: None,
            midi_error: None,
            midi_rec: std::collections::HashMap::new(),
            auto_rec: auto_record::AutoRecorder::default(),
            export_dialog: export_dialog::ExportDialog::default(),
            settings_dialog: settings_dialog::SettingsDialog::default(),
            recovery: Vec::new(),
            error: None,
            broken: false,
            session: None,
            export_task: None,
            was_playing: false,
        };
        if let Some(dir) = app.settings.browser_dir.clone() {
            app.browser.set_dir(&dir);
        }
        if let Some(dir) = Settings::data_dir() {
            let autosave_for = |p: Option<&Path>| {
                let doc = Document::with_path(p.map(Path::to_path_buf));
                doc.autosave_path(Some(&dir))
            };
            app.recovery = session::find_recoveries(&dir, &autosave_for);
            app.session = session::SessionLock::acquire(&dir);
        }
        app.restart_audio();
        if let Some(path) = open {
            app.open_path(&path);
        }
        app
    }

    fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
    }

    fn save_settings(&mut self) {
        if let Some(path) = &self.settings_path {
            if let Err(e) = self.settings.save_to(path) {
                tracing::error!("saving settings failed: {e}");
            }
        }
    }

    // ---- audio -------------------------------------------------------------------------

    fn restart_audio(&mut self) {
        self.output = None;
        let a = &self.settings.audio;
        let request = OutputRequest {
            device_id: a.output_device.clone(),
            sample_rate: a.sample_rate,
            buffer_size: a.buffer_size,
        };
        self.input = None;
        self.input_error = None;
        match audio::start_output(&request) {
            Ok((out, mut engine)) => {
                self.sync.reset();
                self.graph = GraphState::default();
                if a.input_enabled {
                    let rate = out.info().sample_rate;
                    match audio::start_input(a.input_device.as_deref(), rate, a.buffer_size) {
                        Ok((stream, source)) => {
                            engine.send_or_log(musiton_engine::EngineCommand::SetInput(Some(
                                Box::new(source),
                            )));
                            self.input = Some(stream);
                        }
                        Err(e) => {
                            tracing::warn!("audio input failed: {e}");
                            self.input_error = Some(e.to_string());
                        }
                    }
                }
                self.midi = None;
                self.midi_error = None;
                if a.midi_enabled {
                    match musiton_io::midi::start_midi_input(a.midi_input.as_deref()) {
                        Ok((stream, rx)) => {
                            engine.send_or_log(musiton_engine::EngineCommand::SetMidiInput(Some(
                                Box::new(rx),
                            )));
                            self.midi = Some(stream);
                        }
                        Err(e) => self.midi_error = Some(e),
                    }
                }
                self.output = Some((out, engine));
                self.audio_error = None;
            }
            Err(e) => {
                tracing::error!("audio output failed: {e}");
                self.audio_error = Some(e.to_string());
            }
        }
    }

    /// Curves and live spectra of the selected track's EQs.
    fn eq_views(
        &self,
    ) -> std::collections::HashMap<musiton_core::ids::DeviceId, musiton_ui::device_panel::EqView>
    {
        use musiton_devices::effects::eq;
        let mut out = std::collections::HashMap::new();
        let Some(t) = self
            .arrange
            .selected
            .and_then(|id| self.doc.project.track(id))
        else {
            return out;
        };
        let sr = self
            .output
            .as_ref()
            .map_or(48_000.0, |(_, e)| e.sample_rate());
        let freqs = eq::display_freqs(200);
        for d in t.devices.iter().filter(|d| d.type_id() == "eq") {
            let spectrum = self
                .graph
                .info
                .analyzers
                .get(&d.id)
                .map(|a| eq::spectrum(a, &freqs, sr))
                .unwrap_or_default();
            let curve = eq::curve(d.params(), &freqs, sr);
            out.insert(
                d.id,
                musiton_ui::device_panel::EqView {
                    freqs: freqs.clone(),
                    curve,
                    spectrum,
                },
            );
        }
        out
    }

    /// Moves effect `device` from track `from` to the end of track `to`'s chain.
    fn move_device(
        &mut self,
        from: musiton_core::ids::TrackId,
        device: musiton_core::ids::DeviceId,
        to: musiton_core::ids::TrackId,
    ) {
        let p = &mut self.doc.project;
        let (Some(src), Some(dst)) = (p.track(from), p.track(to)) else {
            return;
        };
        let mut src_devices = src.devices.clone();
        let Some(i) = src_devices.iter().position(|d| d.id == device) else {
            return;
        };
        // The instrument of an instrument track stays where it is.
        if i == 0 && src.kind == musiton_project::TrackKind::Instrument {
            return;
        }
        let slot = src_devices.remove(i);
        let mut dst_devices = dst.devices.clone();
        if dst.kind == musiton_project::TrackKind::Instrument && dst_devices.is_empty() {
            let id = musiton_core::ids::DeviceId(p.alloc_id());
            dst_devices.push(musiton_project::DeviceSlot::builtin(
                id,
                musiton_devices::POLYSYNTH,
            ));
        }
        dst_devices.push(slot);
        let edit = musiton_project::Edit::Batch(vec![
            musiton_project::Edit::SetDevices {
                track: from,
                devices: src_devices,
            },
            musiton_project::Edit::SetDevices {
                track: to,
                devices: dst_devices,
            },
        ]);
        match self.doc.perform(edit, false) {
            Ok(()) => self.set_status("Effect moved"),
            Err(e) => self.set_status(format!("Moving the effect failed: {e}")),
        }
    }

    /// Export window, start of an export, and its progress.
    fn export_ui(&mut self, ctx: &egui::Context) {
        let p = &self.doc.project;
        let selection: Vec<(musiton_core::Tick, musiton_core::Tick)> = p
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .filter(|c| self.arrange.selected_clips.contains(&c.id()))
            .map(|c| (c.start(), c.end()))
            .collect();
        let has_loop = p.loop_range.is_some();
        if self
            .export_dialog
            .show(ctx, !selection.is_empty(), has_loop)
        {
            let range = match self.export_dialog.range {
                export::ExportRange::Song => (musiton_core::Tick(0), export::song_end(p)),
                export::ExportRange::Loop => p
                    .loop_range
                    .unwrap_or((musiton_core::Tick(0), export::song_end(p))),
                export::ExportRange::Selection => (
                    selection.iter().map(|s| s.0).min().unwrap_or_default(),
                    selection.iter().map(|s| s.1).max().unwrap_or_default(),
                ),
            };
            let options = self.export_dialog.options();
            let name = self.doc.name();
            let path = if self.export_dialog.stems {
                rfd::FileDialog::new()
                    .set_title("Folder for the stems")
                    .pick_folder()
            } else {
                rfd::FileDialog::new()
                    .set_title("Export audio")
                    .add_filter(options.format.extension(), &[options.format.extension()])
                    .set_file_name(format!("{name}.{}", options.format.extension()))
                    .save_file()
            };
            if let Some(path) = path {
                let job = export::ExportJob {
                    range,
                    tail_secs: self.export_dialog.tail,
                    options,
                    stems: self.export_dialog.stems,
                    path,
                };
                let assets = self.assets.clone();
                self.export_task = Some(export::start(job, self.doc.project.clone(), assets, name));
            }
        }
        let Some(task) = &self.export_task else {
            return;
        };
        ctx.request_repaint_after(Duration::from_millis(50));
        if let Some(result) = task.result() {
            self.export_task = None;
            match result {
                Ok(files) => self.set_status(format!(
                    "Exported {}",
                    files
                        .first()
                        .map_or(String::new(), |f| f.display().to_string())
                        + if files.len() > 1 { " and more" } else { "" }
                )),
                Err(e) => self.set_status(e),
            }
            return;
        }
        let progress = task.progress.load(std::sync::atomic::Ordering::Relaxed) as f32 / 1000.0;
        let mut cancel = false;
        egui::Modal::new(egui::Id::unique("export_progress")).show(ctx, |ui| {
            ui.heading("Exporting…");
            ui.add(
                egui::ProgressBar::new(progress)
                    .show_percentage()
                    .desired_width(300.0),
            );
            cancel = ui.button("Cancel").clicked();
        });
        if cancel {
            task.cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Device types for the device panel (built-ins with factory + user presets).
    fn device_types(&self) -> Vec<musiton_ui::device_panel::DeviceType> {
        musiton_devices::INSTRUMENTS
            .iter()
            .map(|t| {
                let mut presets: Vec<(String, Vec<(u64, f32)>)> = t
                    .presets
                    .iter()
                    .map(|(n, v)| ((*n).to_owned(), v.to_vec()))
                    .collect();
                presets.extend(
                    self.user_presets
                        .get(t.type_id)
                        .cloned()
                        .unwrap_or_default(),
                );
                let sampler = t.type_id == musiton_devices::SAMPLER;
                musiton_ui::device_panel::DeviceType {
                    type_id: t.type_id,
                    name: t.name,
                    params: t.params,
                    uses_samples: t.uses_samples,
                    presets,
                    drum_param: sampler.then_some(musiton_devices::sampler::id::DRUM),
                    root_param: sampler.then_some(musiton_devices::sampler::id::ROOT),
                }
            })
            .collect()
    }

    /// Asks for an audio file and puts it into the track's sampler (pad `key` in drum mode).
    fn load_sample(&mut self, track: musiton_core::ids::TrackId, key: Option<u8>) {
        if self.doc.dir().is_none() {
            self.set_status("Save the project first: samples are copied into its folder.");
            if !self.save_as() {
                return;
            }
        }
        let Some(dir) = self.doc.dir().map(Path::to_path_buf) else {
            return;
        };
        let Some(src) = rfd::FileDialog::new()
            .set_title("Load sample")
            .add_filter("Audio", &musiton_io::file::AUDIO_EXTENSIONS)
            .pick_file()
        else {
            return;
        };
        let imported =
            match musiton_io::file::import_audio(&src, &dir, self.doc.project.sample_rate) {
                Ok(i) => i,
                Err(e) => {
                    self.set_status(format!("Loading the sample failed: {e}"));
                    return;
                }
            };
        let p = &mut self.doc.project;
        let Some(t) = p.track(track) else {
            return;
        };
        let mut devices = t.devices.clone();
        if devices
            .first()
            .is_none_or(|d| d.type_id() != musiton_devices::SAMPLER)
        {
            let d = musiton_project::DeviceSlot::builtin(
                musiton_core::ids::DeviceId(p.alloc_id()),
                musiton_devices::SAMPLER,
            );
            match devices.first_mut() {
                Some(x) => *x = d,
                None => devices.push(d),
            }
        }
        let asset = musiton_core::ids::AssetId(p.alloc_id());
        if let Some(musiton_project::DeviceSlot {
            kind: musiton_project::DeviceKind::Builtin { samples, .. },
            ..
        }) = devices.first_mut()
        {
            let zone_key = key.unwrap_or(60);
            match key {
                Some(k) => samples.retain(|z| z.key != k),
                None => samples.clear(),
            }
            samples.push(musiton_project::SampleZone {
                key: zone_key,
                asset,
            });
        }
        let edits = vec![
            musiton_project::Edit::AddAsset(Some(musiton_project::Asset {
                id: asset,
                file: imported.rel_path.clone(),
            })),
            musiton_project::Edit::SetDevices { track, devices },
        ];
        self.assets.insert(asset, imported.audio);
        match self.doc.perform(musiton_project::Edit::Batch(edits), false) {
            Ok(()) => self.set_status(format!("Loaded {}", src.display())),
            Err(e) => self.set_status(format!("Loading the sample failed: {e}")),
        }
    }

    /// Imports a .mid file: one instrument track with one clip per file track.
    fn import_midi(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Import MIDI file")
            .add_filter("MIDI file", &["mid", "midi"])
            .pick_file()
        else {
            return;
        };
        let (tracks, bpm) = match musiton_io::midi::read_smf(&path) {
            Ok(r) => r,
            Err(e) => {
                self.set_status(format!("MIDI import failed: {e}"));
                return;
            }
        };
        let p = &mut self.doc.project;
        let mut edits = Vec::new();
        // An empty song takes over the file's tempo.
        if let (Some(bpm), true) = (bpm, p.tracks.is_empty()) {
            edits.push(musiton_project::Edit::SetTempo(
                musiton_core::TempoMap::constant(bpm),
            ));
        }
        let bar = p.time_sig.ticks_per_bar();
        let stem = path
            .file_stem()
            .map_or("MIDI".to_owned(), |s| s.to_string_lossy().into_owned());
        let colors = musiton_ui::arrange::TRACK_COLORS;
        for (i, t) in tracks.iter().enumerate() {
            let end = t
                .notes
                .iter()
                .map(|n| n.start + n.length)
                .max()
                .unwrap_or(bar);
            let id = musiton_core::ids::TrackId(p.alloc_id());
            let clip = musiton_project::MidiClip {
                id: musiton_core::ids::ClipId(p.alloc_id()),
                start: musiton_core::Tick(0),
                length: musiton_core::Tick((end + bar - 1) / bar * bar),
                notes: t
                    .notes
                    .iter()
                    .map(|n| musiton_project::Note {
                        pitch: n.key,
                        velocity: n.velocity,
                        start: musiton_core::Tick(n.start),
                        length: musiton_core::Tick(n.length),
                        channel: n.channel,
                    })
                    .collect(),
            };
            edits.push(musiton_project::Edit::AddTrack {
                index: p.tracks.len() + i,
                track: Some(musiton_project::Track {
                    id,
                    name: if t.name.is_empty() {
                        format!("{stem} {}", i + 1)
                    } else {
                        t.name.clone()
                    },
                    kind: musiton_project::TrackKind::Instrument,
                    color: colors[(p.tracks.len() + i) % colors.len()],
                    clips: vec![musiton_project::Clip::Midi(clip)],
                    ..musiton_project::Track::default()
                }),
            });
        }
        if tracks.is_empty() {
            self.set_status("The MIDI file contains no notes.");
            return;
        }
        match self.doc.perform(musiton_project::Edit::Batch(edits), false) {
            Ok(()) => self.set_status(format!(
                "Imported {} track(s) from {}",
                tracks.len(),
                path.display()
            )),
            Err(e) => self.set_status(format!("MIDI import failed: {e}")),
        }
    }

    /// Exports every instrument track's MIDI clips to a .mid file.
    fn export_midi(&mut self) {
        let p = &self.doc.project;
        let tracks: Vec<musiton_io::midi::SmfTrack> = p
            .tracks
            .iter()
            .filter(|t| t.kind == musiton_project::TrackKind::Instrument)
            .map(|t| musiton_io::midi::SmfTrack {
                name: t.name.clone(),
                notes: t
                    .clips
                    .iter()
                    .filter_map(|c| match c {
                        musiton_project::Clip::Midi(m) => Some(m),
                        musiton_project::Clip::Audio(_) => None,
                    })
                    .flat_map(|m| {
                        m.notes.iter().filter(|n| n.start.0 < m.length.0).map(|n| {
                            musiton_io::midi::SmfNote {
                                start: m.start.0 + n.start.0,
                                length: n.length.0.min(m.length.0 - n.start.0),
                                key: n.pitch,
                                velocity: n.velocity,
                                channel: n.channel,
                            }
                        })
                    })
                    .collect(),
            })
            .filter(|t| !t.notes.is_empty())
            .collect();
        if tracks.is_empty() {
            self.set_status("There are no MIDI notes to export.");
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_title("Export MIDI file")
            .add_filter("MIDI file", &["mid"])
            .set_file_name(format!("{}.mid", self.doc.name()))
            .save_file()
        else {
            return;
        };
        match musiton_io::midi::write_smf(&path, p.tempo.bpm_at(0.0), &tracks) {
            Ok(()) => self.set_status(format!("Exported {}", path.display())),
            Err(e) => self.set_status(format!("MIDI export failed: {e}")),
        }
    }

    /// The track that plays live MIDI: the first armed instrument track, else the
    /// selected track if it is an instrument track.
    fn live_midi_track(&self) -> Option<musiton_core::ids::TrackId> {
        let p = &self.doc.project;
        let is_inst = |t: &musiton_project::Track| t.kind == musiton_project::TrackKind::Instrument;
        p.tracks
            .iter()
            .find(|t| is_inst(t) && t.record.armed)
            .or_else(|| {
                self.arrange
                    .selected
                    .and_then(|id| p.track(id))
                    .filter(|t| is_inst(t))
            })
            .map(|t| t.id)
    }

    /// Collects recorded live MIDI; when recording ends, turns it into MIDI clips.
    fn finish_midi_recordings(&mut self) {
        for (track, rx) in &mut self.graph.midi_taps {
            while let Ok(ev) = rx.pop() {
                self.midi_rec.entry(*track).or_default().push(ev);
            }
        }
        let Some((out, engine)) = &self.output else {
            return;
        };
        let shared = engine.shared();
        if (shared.recording() && shared.playing()) || self.midi_rec.is_empty() {
            return;
        }
        let latency = i64::from(out.info().buffer_size.unwrap_or(256));
        let sr = f64::from(engine.sample_rate());
        let stop = shared.playhead();
        let overdub = self.transport.overdub;
        for (track, events) in std::mem::take(&mut self.midi_rec) {
            let p = &mut self.doc.project;
            let Some(edit) = midi_record::to_edit(p, track, &events, latency, stop, sr, overdub)
            else {
                continue;
            };
            if let Err(e) = self.doc.perform(edit, false) {
                self.set_status(format!("Adding the MIDI recording failed: {e}"));
            }
        }
    }

    /// Picks up the result of a running latency measurement.
    fn update_measurement(&mut self) {
        if !self.measuring {
            return;
        }
        let Some((_, e)) = &self.output else {
            self.measuring = false;
            return;
        };
        match e.shared().probe_result() {
            musiton_engine::PROBE_NONE => {}
            musiton_engine::PROBE_TIMEOUT => {
                self.measuring = false;
                self.audio_dialog.measure_status =
                    "No click heard on input 1 — check the cable/microphone and input level."
                        .into();
            }
            frames => {
                self.measuring = false;
                let frames = frames as u32;
                self.settings.audio.measured_latency = Some(frames);
                self.audio_dialog.set_measured(Some(frames));
                self.save_settings();
                self.audio_dialog.measure_status = format!("Round trip: {frames} samples");
            }
        }
    }

    /// Frames recorded audio is moved earlier to line up with what was heard.
    fn record_latency(&self) -> i64 {
        let Some((out, _)) = &self.output else {
            return 0;
        };
        let info = out.info();
        let estimate = info.buffer_size.unwrap_or(256) * 2;
        self.settings
            .audio
            .record_latency(estimate, info.sample_rate)
    }

    /// Turns finished recording takes into clips. A new take mutes the clips it covers.
    fn finish_recordings(&mut self) {
        let mut takes = Vec::new();
        for w in &self.writers {
            takes.extend(w.poll());
        }
        self.writers.retain(|w| !w.finished());
        let latency = self.record_latency();
        for take in takes {
            let take = match take {
                Ok(t) => t,
                Err(e) => {
                    self.set_status(format!("Recording failed: {e}"));
                    continue;
                }
            };
            let track = musiton_core::ids::TrackId(take.key);
            let p = &mut self.doc.project;
            if p.track(track).is_none() {
                continue;
            }
            let sr = f64::from(take.audio.sample_rate.max(1));
            let Some((start, end, skip)) =
                assets::place_take(&p.tempo, take.start, take.audio.frames(), latency, sr)
            else {
                continue;
            };
            let asset = musiton_core::ids::AssetId(p.alloc_id());
            let clip_id = musiton_core::ids::ClipId(p.alloc_id());
            let mut edits = vec![musiton_project::Edit::AddAsset(Some(
                musiton_project::Asset {
                    id: asset,
                    file: format!("audio/{}", take.file_name),
                },
            ))];
            // Mute what the new take covers (older takes stay available).
            if let Some(t) = p.track(track) {
                for c in &t.clips {
                    if let musiton_project::Clip::Audio(a) = c {
                        let overlaps = a.start.0 < end.0 && a.start.0 + a.length.0 > start.0;
                        if overlaps && !a.muted {
                            let mut m = a.clone();
                            m.muted = true;
                            edits.push(musiton_project::Edit::ReplaceClip {
                                track,
                                clip: musiton_project::Clip::Audio(m),
                            });
                        }
                    }
                }
            }
            edits.push(musiton_project::Edit::AddClip {
                track,
                clip: Some(musiton_project::Clip::Audio(musiton_project::AudioClip {
                    id: clip_id,
                    asset,
                    start,
                    length: musiton_core::Tick(end.0 - start.0),
                    source_offset: musiton_core::SamplePos(skip),
                    ..musiton_project::AudioClip::default()
                })),
            });
            self.assets.insert(asset, take.audio);
            match self.doc.perform(musiton_project::Edit::Batch(edits), false) {
                Ok(()) => self.set_status(format!("Recorded {}", take.file_name)),
                Err(e) => self.set_status(format!("Adding the recording failed: {e}")),
            }
        }
    }

    fn apply_audio_settings(&mut self, audio: AudioSettings) {
        self.settings.audio = audio;
        self.save_settings();
        self.restart_audio();
    }

    fn stop_and_rewind(&mut self) {
        if let Some((_, e)) = &mut self.output {
            e.send_or_log(musiton_engine::EngineCommand::Stop);
            e.send_or_log(musiton_engine::EngineCommand::Locate(
                musiton_core::SamplePos(0),
            ));
        }
    }

    // ---- files -------------------------------------------------------------------------

    /// Runs `action` now, or asks about unsaved changes first.
    fn request(&mut self, action: Pending) {
        if self.doc.is_dirty() {
            self.pending = Some(action);
        } else {
            self.run(action);
        }
    }

    fn run(&mut self, action: Pending) {
        match action {
            Pending::New => {
                self.stop_and_rewind();
                self.doc = Document::default();
                self.load_assets();
            }
            Pending::Open(Some(path)) => self.open_path(&path),
            Pending::Open(None) => {
                let mut dialog = rfd::FileDialog::new()
                    .set_title("Open project")
                    .add_filter("Musiton project", &[EXTENSION]);
                if let Some(dir) = self.default_dir() {
                    dialog = dialog.set_directory(dir);
                }
                if let Some(path) = dialog.pick_file() {
                    self.open_path(&path);
                }
            }
            Pending::Quit => {
                self.allow_close = true;
            }
        }
    }

    fn default_dir(&self) -> Option<PathBuf> {
        let recent = self.settings.recent_projects.first()?;
        recent.parent()?.parent().map(Path::to_path_buf)
    }

    /// Copies the demo song to the Documents folder (first time) and opens it.
    fn open_demo(&mut self) {
        let parent = directories::UserDirs::new()
            .and_then(|d| d.document_dir().map(Path::to_path_buf))
            .or_else(Settings::data_dir);
        let Some(parent) = parent else {
            self.error = Some("No folder found to put the demo song in.".into());
            return;
        };
        match demo::install(&parent) {
            Ok(path) => self.request(Pending::Open(Some(path))),
            Err(e) => self.error = Some(format!("Could not create the demo song: {e}")),
        }
    }

    fn open_path(&mut self, path: &Path) {
        match Document::open(path) {
            Ok(doc) => {
                self.stop_and_rewind();
                self.doc = doc;
                self.settings.add_recent(path);
                self.save_settings();
                self.set_status(format!("Opened {}", path.display()));
                self.load_assets();
            }
            Err(e) => {
                tracing::error!("opening {} failed: {e}", path.display());
                self.error = Some(format!("Could not open {}:\n{e}", path.display()));
            }
        }
    }

    /// Saves; asks for a location if the song was never saved. Returns `true` on success.
    fn save(&mut self) -> bool {
        match self.doc.path.clone() {
            Some(path) => self.save_to(&path),
            None => self.save_as(),
        }
    }

    fn save_as(&mut self) -> bool {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Save project as")
            .add_filter("Musiton project", &[EXTENSION])
            .set_file_name(format!("{}.{EXTENSION}", self.doc.name()));
        if let Some(dir) = self.default_dir() {
            dialog = dialog.set_directory(dir);
        }
        match dialog.save_file() {
            Some(chosen) => self.save_to(&musiton_project::project_dir_for(&chosen)),
            None => false,
        }
    }

    fn save_to(&mut self, path: &Path) -> bool {
        match self.doc.save_to(path) {
            Ok(()) => {
                self.settings.add_recent(path);
                self.save_settings();
                self.set_status(format!("Saved {}", path.display()));
                true
            }
            Err(e) => {
                tracing::error!("saving {} failed: {e}", path.display());
                self.set_status(format!("Could not save: {e}"));
                false
            }
        }
    }

    fn undo(&mut self) {
        match self.doc.undo() {
            Ok(Some(label)) => self.set_status(format!("Undo: {label}")),
            Ok(None) => {}
            Err(e) => self.set_status(format!("Undo failed: {e}")),
        }
    }

    fn redo(&mut self) {
        match self.doc.redo() {
            Ok(Some(label)) => self.set_status(format!("Redo: {label}")),
            Ok(None) => {}
            Err(e) => self.set_status(format!("Redo failed: {e}")),
        }
    }

    // ---- UI ----------------------------------------------------------------------------

    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, KeyboardShortcut, Modifiers};
        let cmd = Modifiers::COMMAND;
        let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        let pressed = |m, k| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)));
        // Shift variants first: plain Ctrl+S would also match Ctrl+Shift+S.
        if pressed(cmd_shift, Key::S) {
            self.save_as();
        } else if pressed(cmd, Key::S) {
            self.save();
        }
        if pressed(cmd_shift, Key::Z) || pressed(cmd, Key::Y) {
            self.redo();
        } else if pressed(cmd, Key::Z) {
            self.undo();
        }
        if pressed(cmd, Key::N) {
            self.request(Pending::New);
        }
        if pressed(cmd, Key::O) {
            self.request(Pending::Open(None));
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("New            Ctrl+N").clicked() {
                    self.request(Pending::New);
                }
                if ui.button("Open…          Ctrl+O").clicked() {
                    self.request(Pending::Open(None));
                }
                ui.menu_button("Open recent", |ui| {
                    if self.settings.recent_projects.is_empty() {
                        ui.weak("(none)");
                    }
                    for path in self.settings.recent_projects.clone() {
                        if ui.button(path.display().to_string()).clicked() {
                            self.request(Pending::Open(Some(path)));
                        }
                    }
                });
                ui.separator();
                if ui.button("Save           Ctrl+S").clicked() {
                    self.save();
                }
                if ui.button("Save as…       Ctrl+Shift+S").clicked() {
                    self.save_as();
                }
                ui.separator();
                if ui.button("Export audio…").clicked() {
                    self.export_dialog.open = true;
                    ui.close();
                }
                ui.separator();
                if ui.button("Import MIDI file…").clicked() {
                    self.import_midi();
                    ui.close();
                }
                if ui.button("Export MIDI file…").clicked() {
                    self.export_midi();
                    ui.close();
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Edit", |ui| {
                let undo = self.doc.undo_label();
                if ui
                    .add_enabled(
                        undo.is_some(),
                        egui::Button::new(format!("Undo {}", undo.unwrap_or(""))),
                    )
                    .on_hover_text("Ctrl+Z")
                    .clicked()
                {
                    self.undo();
                }
                let redo = self.doc.redo_label();
                if ui
                    .add_enabled(
                        redo.is_some(),
                        egui::Button::new(format!("Redo {}", redo.unwrap_or(""))),
                    )
                    .on_hover_text("Ctrl+Y / Ctrl+Shift+Z")
                    .clicked()
                {
                    self.redo();
                }
            });
            ui.menu_button("Options", |ui| {
                if ui.button("Settings…").clicked() {
                    self.settings_dialog.open = true;
                    ui.close();
                }
                if ui.button("Audio settings…").clicked() {
                    self.audio_dialog.open(&self.settings.audio);
                    ui.close();
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("Open demo song").clicked() {
                    self.open_demo();
                }
                if ui.button("Keyboard shortcuts").clicked() {
                    self.show_shortcuts = true;
                }
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let mut restart = false;
        ui.horizontal(|ui| {
            if let Some(err) = &self.audio_error {
                ui.colored_label(ui.visuals().error_fg_color, format!("Audio: {err}"));
            } else if let Some((out, engine)) = &self.output {
                if out.has_failed() {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        "Audio stopped working — check Options > Audio settings",
                    );
                    restart = ui.small_button("Restart audio").clicked();
                } else {
                    let info = out.info();
                    ui.label(&info.device_name);
                    ui.separator();
                    ui.label(format!("{} Hz", info.sample_rate));
                    ui.separator();
                    match info.buffer_size {
                        Some(b) => ui.label(format!("{b} samples")),
                        None => ui.label("default buffer"),
                    };
                    ui.separator();
                    let shared = engine.shared();
                    let load = shared.cpu_load();
                    ui.add(
                        egui::ProgressBar::new(load.clamp(0.0, 1.0))
                            .desired_width(80.0)
                            .text(format!("CPU {:.0} %", load * 100.0)),
                    );
                    ui.separator();
                    ui.label(format!("Dropouts: {}", shared.xruns()));
                    if ui.small_button("reset").clicked() {
                        shared.reset_xruns();
                    }
                }
            } else {
                ui.label("Audio: off");
            }
            if let Some(err) = &self.input_error {
                ui.separator();
                ui.colored_label(ui.visuals().warn_fg_color, format!("Input: {err}"));
            } else if let Some(input) = &self.input {
                ui.separator();
                ui.label(format!("In: {} ({} ch)", input.device_name, input.channels));
            }
            if let Some(err) = &self.midi_error {
                ui.separator();
                ui.weak(format!("MIDI: {err}"));
            } else if let Some(m) = &self.midi {
                ui.separator();
                let cc = self.output.as_ref().and_then(|(_, e)| e.shared().last_cc());
                match cc {
                    Some((ch, cc, v)) => {
                        ui.label(format!("MIDI: {} · CC{cc} ch{} = {v}", m.name, ch + 1))
                    }
                    None => ui.label(format!("MIDI: {}", m.name)),
                };
            }
            let lost = self
                .graph
                .record_dropped
                .load(std::sync::atomic::Ordering::Relaxed);
            if lost > 0 {
                ui.separator();
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Recording: {lost} chunk(s) lost (disk too slow)"),
                );
            }
            if let Some((msg, at)) = &self.status {
                if at.elapsed() < STATUS_TIME {
                    ui.separator();
                    ui.label(msg);
                }
            }
        });
        if restart {
            self.restart_audio();
        }
    }

    fn unsaved_dialog(&mut self, ctx: &egui::Context) {
        let Some(action) = self.pending.clone() else {
            return;
        };
        let mut choice = None;
        egui::Modal::new(egui::Id::unique("unsaved_changes")).show(ctx, |ui| {
            ui.heading("Unsaved changes");
            ui.label(format!("Save changes to \"{}\"?", self.doc.name()));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    choice = Some(0);
                }
                if ui.button("Don't save").clicked() {
                    choice = Some(1);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(2);
                }
            });
        });
        match choice {
            Some(0) => {
                self.pending = None;
                if self.save() {
                    self.run(action);
                }
            }
            Some(1) => {
                self.pending = None;
                self.run(action);
            }
            Some(_) => self.pending = None,
            None => {}
        }
        if self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Loads the audio files of the current project and resets clip selection.
    fn load_assets(&mut self) {
        self.arrange.selected_clips.clear();
        self.arrange.selected = None;
        let failed = self.assets.load_project(&self.doc.project, self.doc.dir());
        if !failed.is_empty() {
            self.set_status(format!(
                "{} audio file(s) missing: {}",
                failed.len(),
                failed.join(", ")
            ));
        }
    }

    /// Starts importing `files`; the first goes to `track`, others onto new tracks.
    fn import(
        &mut self,
        files: Vec<PathBuf>,
        track: Option<musiton_core::ids::TrackId>,
        at: musiton_core::Tick,
    ) {
        if self.doc.dir().is_none() {
            self.set_status("Save the project first: imported audio is copied into its folder.");
            if !self.save_as() {
                return;
            }
        }
        let Some(dir) = self.doc.dir().map(Path::to_path_buf) else {
            return;
        };
        for (i, file) in files.into_iter().enumerate() {
            if !musiton_io::file::is_audio_file(&file) {
                self.set_status(format!("Not an audio file: {}", file.display()));
                continue;
            }
            let target = ImportTarget {
                track: if i == 0 { track } else { None },
                at,
            };
            self.importer
                .start(file, dir.clone(), self.doc.project.sample_rate, target);
        }
    }

    /// Turns finished imports into clips (one undo step per file).
    fn finish_imports(&mut self) {
        for ImportDone {
            source,
            target,
            result,
        } in self.importer.finished()
        {
            let imported = match result {
                Ok(i) => i,
                Err(e) => {
                    self.set_status(format!("Import of {} failed: {e}", source.display()));
                    continue;
                }
            };
            let p = &mut self.doc.project;
            let asset = musiton_core::ids::AssetId(p.alloc_id());
            let clip_id = musiton_core::ids::ClipId(p.alloc_id());
            let sr = f64::from(imported.audio.sample_rate);
            let end = musiton_ui::clip_edit::tick_after_frames(
                &p.tempo,
                target.at,
                imported.audio.frames() as f64,
                sr,
            );
            let mut edits = vec![musiton_project::Edit::AddAsset(Some(
                musiton_project::Asset {
                    id: asset,
                    file: imported.rel_path.clone(),
                },
            ))];
            let track = match target.track.filter(|t| p.track(*t).is_some()) {
                Some(t) => t,
                None => {
                    let id = musiton_core::ids::TrackId(p.alloc_id());
                    let name = source
                        .file_stem()
                        .map_or("Audio".to_owned(), |s| s.to_string_lossy().into_owned());
                    let colors = musiton_ui::arrange::TRACK_COLORS;
                    edits.push(musiton_project::Edit::AddTrack {
                        index: p.tracks.len(),
                        track: Some(musiton_project::Track {
                            id,
                            name,
                            color: colors[p.tracks.len() % colors.len()],
                            ..musiton_project::Track::default()
                        }),
                    });
                    id
                }
            };
            edits.push(musiton_project::Edit::AddClip {
                track,
                clip: Some(musiton_project::Clip::Audio(musiton_project::AudioClip {
                    id: clip_id,
                    asset,
                    start: target.at,
                    length: musiton_core::Tick((end.0 - target.at.0).max(1)),
                    ..musiton_project::AudioClip::default()
                })),
            });
            self.assets.insert(asset, imported.audio);
            match self.doc.perform(musiton_project::Edit::Batch(edits), false) {
                Ok(()) => self.set_status(format!("Imported {}", source.display())),
                Err(e) => self.set_status(format!("Import failed: {e}")),
            }
        }
    }

    /// Rebuilds the engine graph when the project structure or the loaded audio changed,
    /// and sends changed mixer values as parameter commands.
    fn update_graph(&mut self) {
        let live = self.live_midi_track();
        let Some((_, engine)) = &mut self.output else {
            return;
        };
        if self.graph.revision != Some(self.doc.revision)
            || self.graph.asset_gen != self.assets.generation
        {
            self.graph.revision = Some(self.doc.revision);
            let input_channels = self.input.as_ref().map_or(0, |i| usize::from(i.channels));
            let structure = engine_graph::structure(&self.doc.project, input_channels);
            if self.graph.structure.as_ref() != Some(&structure)
                || self.graph.asset_gen != self.assets.generation
            {
                self.graph.asset_gen = self.assets.generation;
                self.graph.structure = Some(structure);
                match engine_graph::build(
                    &self.doc.project,
                    &self.assets,
                    engine.sample_rate(),
                    engine.max_block(),
                    input_channels,
                ) {
                    Ok(built) => {
                        if !built.record_taps.is_empty() {
                            match self.doc.dir() {
                                Some(dir) => {
                                    let sr = engine.sample_rate() as u32;
                                    let jobs = built
                                        .record_taps
                                        .into_iter()
                                        .map(|tap| {
                                            let name = self
                                                .doc
                                                .project
                                                .track(tap.track)
                                                .map_or("Recording".to_owned(), |t| t.name.clone());
                                            musiton_io::recording::RecordJob {
                                                key: tap.track.0,
                                                rx: tap.rx,
                                                dir: dir.join("audio"),
                                                name,
                                                channels: tap.channels,
                                                sample_rate: sr,
                                            }
                                        })
                                        .collect();
                                    self.writers
                                        .push(musiton_io::recording::DiskWriter::spawn(jobs));
                                }
                                None => {
                                    self.status = Some((
                                        "Save the project before recording (audio goes into its folder)."
                                            .into(),
                                        std::time::Instant::now(),
                                    ));
                                }
                            }
                        }
                        self.graph.record_dropped = built.record_dropped;
                        engine.send_or_log(musiton_engine::EngineCommand::SwapGraph(Box::new(
                            built.graph,
                        )));
                        self.graph.sent = built.info.params(&self.doc.project, live);
                        self.graph.sent_notes = engine_graph::all_notes(&self.doc.project);
                        self.graph.sent_automation = None;
                        self.graph.midi_taps = built.midi_taps;
                        self.graph.info = built.info;
                    }
                    Err(e) => tracing::error!("building the engine graph failed: {e}"),
                }
            }
        }
        if self.graph.notes_rev != Some(self.doc.revision) {
            self.graph.notes_rev = Some(self.doc.revision);
            for (track, notes) in engine_graph::all_notes(&self.doc.project) {
                if self.graph.sent_notes.get(&track) == Some(&notes) {
                    continue;
                }
                if let Some(node) = self.graph.info.midi_node(track) {
                    self.graph.sent_notes.insert(track, notes.clone());
                    engine.send_or_log(musiton_engine::EngineCommand::NodeData {
                        node,
                        data: Box::new(musiton_engine::nodes::MidiNotes(notes)),
                    });
                }
            }
        }
        for (key, value) in self.graph.info.params(&self.doc.project, live) {
            if self.graph.sent.get(&key) != Some(&value) {
                self.graph.sent.insert(key, value);
                engine.send_or_log(musiton_engine::EngineCommand::SetParam {
                    node: key.0,
                    id: key.1,
                    val: value,
                });
            }
        }
    }

    /// Sends the automation for the running graph when it changed.
    fn sync_automation(&mut self) {
        let Some((_, engine)) = &mut self.output else {
            return;
        };
        let auto = self
            .graph
            .info
            .automation(&self.doc.project, &self.auto_rec.active());
        if self.graph.sent_automation.as_ref() != Some(&auto) {
            self.graph.sent_automation = Some(auto.clone());
            let set = (!auto.0.is_empty()).then(|| Box::new(auto));
            engine.send_or_log(musiton_engine::EngineCommand::SetAutomation(set));
        }
    }

    /// Current normalized value of an automation target (its static setting).
    fn target_value(
        &self,
        track: musiton_core::ids::TrackId,
        target: musiton_project::ParamTarget,
    ) -> f32 {
        use musiton_project::ParamTarget;
        let p = &self.doc.project;
        let Some(t) = p.track(track) else {
            return 0.0;
        };
        match target {
            ParamTarget::Volume => engine_graph::VOLUME_PARAM.to_normalized(t.strip.gain_db),
            ParamTarget::Pan => engine_graph::PAN_PARAM.to_normalized(t.strip.pan),
            ParamTarget::Device { device, param } => {
                let info = engine_graph::automation_targets(t)
                    .into_iter()
                    .find(|(x, _, _)| *x == target)
                    .map(|(_, _, i)| i);
                let value = t
                    .devices
                    .iter()
                    .find(|d| d.id == device)
                    .and_then(|d| d.param(param));
                info.map_or(0.0, |i| i.to_normalized(value.unwrap_or(i.default)))
            }
        }
    }

    /// While playing, a control move on a lane in Write/Touch/Latch mode is recorded
    /// (and played directly) instead of changing the static setting. Returns `true` if
    /// the edit was taken.
    fn record_automation(&mut self, edit: &musiton_project::Edit) -> bool {
        use musiton_project::{Edit, ParamTarget};
        let Some((_, engine)) = &self.output else {
            return false;
        };
        if !engine.shared().playing() {
            return false;
        }
        let p = &self.doc.project;
        let mut moves: Vec<(musiton_core::ids::TrackId, ParamTarget, f32)> = Vec::new();
        match edit {
            Edit::SetDeviceParam {
                track,
                device,
                param,
                value,
            } => {
                let target = ParamTarget::Device {
                    device: *device,
                    param: *param,
                };
                if let Some((_, _, info, _)) = self.graph.info.resolve(p, *track, target) {
                    moves.push((
                        *track,
                        target,
                        info.to_normalized(value.unwrap_or(info.default)),
                    ));
                }
            }
            Edit::SetStrip {
                track: Some(track),
                strip,
            } => {
                if let Some(t) = p.track(*track) {
                    if strip.gain_db != t.strip.gain_db {
                        moves.push((
                            *track,
                            ParamTarget::Volume,
                            engine_graph::VOLUME_PARAM.to_normalized(strip.gain_db),
                        ));
                    }
                    if strip.pan != t.strip.pan {
                        moves.push((
                            *track,
                            ParamTarget::Pan,
                            engine_graph::PAN_PARAM.to_normalized(strip.pan),
                        ));
                    }
                }
            }
            _ => {}
        }
        let moves: Vec<_> = moves
            .into_iter()
            .filter(|(t, target, _)| {
                auto_record::AutoRecorder::recording_mode(p, *t, *target).is_some()
            })
            .collect();
        if moves.is_empty() {
            return false;
        }
        let sr = f64::from(engine.sample_rate());
        let tick = musiton_core::Tick(
            p.tempo
                .sample_to_tick(engine.shared().playhead() as f64, sr)
                .round() as i64,
        );
        for (track, target, value) in moves {
            self.auto_rec
                .touch(&self.doc.project, track, target, value, tick);
            if let Some((node, id, info, db_to_gain)) =
                self.graph.info.resolve(&self.doc.project, track, target)
            {
                let plain = info.from_normalized(value);
                let val = if db_to_gain {
                    musiton_core::units::db_to_gain(plain)
                } else {
                    plain
                };
                if let Some((_, e)) = &mut self.output {
                    e.send_or_log(musiton_engine::EngineCommand::SetParam { node, id, val });
                }
                self.graph.sent.remove(&(node, id));
            }
        }
        self.sync_automation();
        true
    }

    /// Starts Write lanes when playback starts, finishes takes on release/stop.
    fn update_automation_recording(&mut self, ctx: &egui::Context) {
        let (playing, tick) = match &self.output {
            Some((_, e)) => {
                let sr = f64::from(e.sample_rate());
                let t = self
                    .doc
                    .project
                    .tempo
                    .sample_to_tick(e.shared().playhead() as f64, sr);
                (e.shared().playing(), musiton_core::Tick(t.round() as i64))
            }
            None => (false, musiton_core::Tick(0)),
        };
        let edits = if playing && !self.was_playing {
            let mut rec = std::mem::take(&mut self.auto_rec);
            let current = |t, x| self.target_value(t, x);
            rec.start_write_lanes(&self.doc.project, &current, tick);
            self.auto_rec = rec;
            Vec::new()
        } else if playing {
            let down = ctx.input(|i| i.pointer.primary_down());
            self.auto_rec.frame(&self.doc.project, tick, down)
        } else if self.was_playing {
            self.auto_rec.stop(&self.doc.project)
        } else {
            Vec::new()
        };
        self.was_playing = playing;
        for e in edits {
            if let Err(e) = self.doc.perform(e, false) {
                self.set_status(format!("Recording automation failed: {e}"));
            }
        }
        self.sync_automation();
    }

    fn apply_actions(&mut self, actions: Vec<UiAction>) {
        for action in actions {
            match action {
                UiAction::Edit { edit, merge } => {
                    if self.record_automation(&edit) {
                        continue;
                    }
                    if let Err(e) = self.doc.perform(edit, merge) {
                        self.set_status(format!("Edit failed: {e}"));
                    }
                }
                UiAction::Import { files, track, at } => self.import(files, track, at),
                UiAction::PreviewNote { on, key } => {
                    if let Some((_, e)) = &mut self.output {
                        e.send_or_log(musiton_engine::EngineCommand::PreviewNote {
                            on,
                            key,
                            vel: 0.8,
                        });
                    }
                }
                UiAction::NewDevice { track, mut devices } => {
                    let existing: Vec<_> = self
                        .doc
                        .project
                        .track(track)
                        .map(|t| t.devices.iter().map(|d| d.id).collect())
                        .unwrap_or_default();
                    for d in &mut devices {
                        if !existing.contains(&d.id) {
                            d.id = musiton_core::ids::DeviceId(self.doc.project.alloc_id());
                        }
                    }
                    if let Err(e) = self
                        .doc
                        .perform(musiton_project::Edit::SetDevices { track, devices }, false)
                    {
                        self.set_status(format!("Edit failed: {e}"));
                    }
                }
                UiAction::LoadSample { track, key } => self.load_sample(track, key),
                UiAction::MoveDevice { from, device, to } => self.move_device(from, device, to),
                UiAction::SavePreset {
                    type_id,
                    name,
                    params,
                } => match presets::save(&type_id, &name, &params) {
                    Ok(()) => {
                        self.user_presets = presets::load_all();
                        self.set_status(format!("Saved preset \"{name}\""));
                    }
                    Err(e) => self.set_status(format!("Saving the preset failed: {e}")),
                },
                UiAction::OpenEditor { track, clip } => {
                    self.piano_roll.open(track, clip);
                    self.arrange.selected = Some(track);
                    self.bottom_tab = BottomTab::Editor;
                }
                UiAction::Locate(tick) => {
                    if let Some((_, engine)) = &mut self.output {
                        let sr = f64::from(engine.sample_rate());
                        let pos = self.doc.project.tempo.tick_to_sample(tick.0 as f64, sr);
                        engine.send_or_log(musiton_engine::EngineCommand::Locate(
                            musiton_core::SamplePos(pos.round().max(0.0) as u64),
                        ));
                    }
                }
            }
        }
    }

    /// Playhead in ticks and whether the transport runs.
    fn playhead(&self) -> (Option<f64>, bool) {
        match &self.output {
            Some((_, e)) => {
                let sr = f64::from(e.sample_rate());
                let tick = self
                    .doc
                    .project
                    .tempo
                    .sample_to_tick(e.shared().playhead() as f64, sr);
                (Some(tick), e.shared().playing())
            }
            None => (None, false),
        }
    }

    fn bottom_panel(&mut self, ui: &mut egui::Ui) -> Vec<UiAction> {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.bottom_tab, BottomTab::Editor, "Editor");
            ui.selectable_value(&mut self.bottom_tab, BottomTab::Device, "Device");
            ui.selectable_value(&mut self.bottom_tab, BottomTab::Mixer, "Mixer");
        });
        ui.separator();
        match self.bottom_tab {
            BottomTab::Editor => self.piano_roll.ui(ui, &self.doc.project),
            BottomTab::Device => {
                let types = self.device_types();
                let project = &self.doc.project;
                let name = |id: musiton_core::ids::AssetId| {
                    project
                        .assets
                        .iter()
                        .find(|a| a.id == id)
                        .and_then(|a| Path::new(&a.file).file_stem())
                        .map_or("?".to_owned(), |s| s.to_string_lossy().into_owned())
                };
                ui.ctx().request_repaint_after(Duration::from_millis(50));
                let effects = effect_types();
                let eq_views = self.eq_views();
                let input = musiton_ui::device_panel::PanelInput {
                    track: self.arrange.selected,
                    instruments: &types,
                    effects: &effects,
                    asset_name: &name,
                    eq_views: &eq_views,
                    playhead: self.playhead().0,
                };
                let project = &self.doc.project;
                egui::ScrollArea::vertical()
                    .show(ui, |ui| self.device_panel.ui(ui, project, &input))
                    .inner
            }
            BottomTab::Mixer => {
                ui.ctx().request_repaint_after(Duration::from_millis(33));
                let levels = self
                    .graph
                    .info
                    .meters
                    .iter()
                    .map(|(k, m)| (*k, (m.take_peak(), m.rms())))
                    .collect();
                self.mixer.playhead = self.playhead().0;
                self.mixer.input_channels =
                    self.input.as_ref().map_or(0, |i| usize::from(i.channels));
                self.mixer.ui(ui, &self.doc.project, &levels)
            }
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let dirty = if self.doc.is_dirty() { "*" } else { "" };
        let title = format!("{}{dirty} — {APP_NAME}", self.doc.name());
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}

impl App {
    /// One frame of the normal interface.
    fn frame_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ctx.request_repaint_after(Duration::from_millis(100));

        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            if self.doc.is_dirty() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.pending = Some(Pending::Quit);
            } else {
                self.allow_close = true;
            }
        }
        if self.pending.is_none() {
            self.shortcuts(&ctx);
        }

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        let mut out = transport::TransportOutput::default();
        egui::Panel::top("transport").show(ui, |ui| {
            let engine = self.output.as_mut().map(|(_, e)| e);
            out = self.transport.ui(ui, &mut self.doc.project, engine);
        });
        for (edit, merge) in out.edits {
            if let Err(e) = self.doc.perform(edit, merge) {
                self.set_status(format!("Edit failed: {e}"));
            }
        }
        if out.settings_changed {
            self.doc.settings_changed();
        }
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        let mut bottom_actions = Vec::new();
        egui::Panel::bottom("bottom")
            .resizable(true)
            .default_size(340.0)
            .min_size(80.0)
            .show(ui, |ui| {
                bottom_actions = self.bottom_panel(ui);
                // Keep the panel at its (resizable) height even with little content.
                ui.allocate_space(ui.available_size());
            });
        egui::Panel::left("browser")
            .resizable(true)
            .default_size(200.0)
            .min_size(120.0)
            .show(ui, |ui| match self.browser.ui(ui) {
                Some(BrowserAction::FolderChanged(dir)) => {
                    self.settings.browser_dir = Some(dir);
                    self.save_settings();
                }
                Some(BrowserAction::Import(file)) => {
                    let at = self.playhead().0.unwrap_or(0.0);
                    let at = self.arrange.snap.floor(at, self.doc.project.time_sig);
                    self.import(vec![file], None, at);
                }
                None => {}
            });
        let (playhead, playing) = self.playhead();
        let mut actions = Vec::new();
        egui::CentralPanel::default_margins().show(ui, |ui| {
            let auto_targets: std::collections::HashMap<_, _> = self
                .doc
                .project
                .tracks
                .iter()
                .map(|t| (t.id, engine_graph::automation_targets(t)))
                .collect();
            let input = ArrangeInput {
                playhead,
                playing,
                peaks: self.assets.peaks(),
                auto_targets: &auto_targets,
            };
            actions = self.arrange.ui(ui, &mut self.doc.project, &input);
        });
        self.apply_actions(actions);
        self.apply_actions(bottom_actions);
        self.finish_imports();
        self.finish_recordings();
        self.finish_midi_recordings();
        self.update_automation_recording(&ctx);
        self.update_graph();
        if self.importer.running() > 0 {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        egui::Window::new("Keyboard shortcuts")
            .open(&mut self.show_shortcuts)
            .collapsible(false)
            .show(&ctx, musiton_ui::shortcuts::shortcuts_table);

        self.update_measurement();
        match self.audio_dialog.show(&ctx) {
            Some(audio_dialog::DialogAction::Apply(new)) => self.apply_audio_settings(new),
            Some(audio_dialog::DialogAction::Measure) => {
                if let Some((_, e)) = &self.output {
                    if self.input.is_some() {
                        e.shared().request_latency_probe();
                        self.measuring = true;
                        self.audio_dialog.measure_status = "Measuring…".into();
                    } else {
                        self.audio_dialog.measure_status = "No input device is running.".into();
                    }
                }
            }
            Some(audio_dialog::DialogAction::Offset(ms)) => {
                self.settings.audio.record_offset_ms = ms;
                self.save_settings();
            }
            None => {}
        }
        self.export_ui(&ctx);
        settings_dialog::apply(&ctx, &self.settings.ui);
        match self.settings_dialog.show(&ctx, &self.settings.ui) {
            Some(settings_dialog::SettingsAction::Changed(ui_settings)) => {
                self.settings.ui = ui_settings;
                self.save_settings();
            }
            Some(settings_dialog::SettingsAction::OpenAudio) => {
                self.audio_dialog.open(&self.settings.audio)
            }
            None => {}
        }
        self.unsaved_dialog(&ctx);

        if let Some((_, engine)) = &mut self.output {
            self.sync.sync(&self.doc.project, engine);
        }
        let interval = Duration::from_secs(u64::from(self.settings.ui.autosave_minutes) * 60);
        match self
            .doc
            .autosave_if_due(Settings::data_dir().as_deref(), interval)
        {
            Some(Ok(path)) => tracing::info!("autosaved to {}", path.display()),
            Some(Err(e)) => self.set_status(format!("Autosave failed: {e}")),
            None => {}
        }
        self.update_title(&ctx);
    }

    /// Keeps the crash-detection lock file pointing at the open project.
    fn update_lock(&mut self) {
        if let Some(lock) = &mut self.session {
            lock.set_project(self.doc.path.as_deref());
        }
    }

    /// Offers to restore the autosave left by a crashed session.
    fn recovery_dialog(&mut self, ctx: &egui::Context) {
        let Some(r) = self.recovery.first() else {
            return;
        };
        let text = match r.project.as_deref().and_then(Path::file_stem) {
            Some(n) => format!(
                "An automatic backup of {} is newer than the saved version. Restore it?",
                n.to_string_lossy()
            ),
            None => "An automatic backup of an unsaved song was found. Restore it?".to_owned(),
        };
        let mut choice = None;
        egui::Modal::new(egui::Id::unique("recovery")).show(ctx, |ui| {
            ui.heading("Musiton did not close properly");
            ui.label(text.as_str());
            ui.horizontal(|ui| {
                if ui.button("Restore backup").clicked() {
                    choice = Some(true);
                }
                if ui.button("Discard").clicked() {
                    choice = Some(false);
                }
            });
        });
        let Some(restore) = choice else { return };
        let r = self.recovery.remove(0);
        if restore {
            // Restoring replaces the open song, so further backups are not offered.
            self.recovery.clear();
            match Document::recover(&r.autosave, r.project) {
                Ok(doc) => {
                    self.stop_and_rewind();
                    self.doc = doc;
                    self.load_assets();
                    self.set_status("Backup restored – save to keep it.");
                }
                Err(e) => self.error = Some(format!("The backup could not be restored: {e}")),
            }
        }
    }

    /// Shows the current error message (if any).
    fn error_dialog(&mut self, ctx: &egui::Context) {
        let Some(msg) = &self.error else { return };
        let broken = self.broken;
        let mut close = false;
        egui::Modal::new(egui::Id::unique("error")).show(ctx, |ui| {
            ui.heading(if broken {
                "Something went wrong"
            } else {
                "Error"
            });
            ui.label(msg.as_str());
            ui.horizontal(|ui| {
                if broken {
                    if ui.button("Quit Musiton").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                } else if ui.button("OK").clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.error = None;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if !self.broken {
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.frame_ui(ui);
                self.update_lock();
                self.recovery_dialog(&ctx);
            }));
            if let Err(payload) = run {
                let what = payload
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_owned())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown error".into());
                tracing::error!("UI panic: {what}");
                self.broken = true;
                self.allow_close = true;
                let saved = self.doc.save_emergency(Settings::data_dir().as_deref());
                self.error = Some(match saved {
                    Some(p) => format!(
                        "Musiton hit an internal error ({what}).\n\nYour song was saved to\n{}\nPlease restart Musiton.",
                        p.display()
                    ),
                    None => format!(
                        "Musiton hit an internal error ({what}) and could not save a backup.\nPlease restart Musiton."
                    ),
                });
            }
        }
        self.error_dialog(&ctx);
    }

    fn on_exit(&mut self) {
        // A broken session keeps its lock so the emergency save is offered next start.
        if !self.broken {
            if let Some(lock) = self.session.take() {
                lock.release();
            }
        }
    }
}
