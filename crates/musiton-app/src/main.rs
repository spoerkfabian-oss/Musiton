//! Musiton — a digital audio workstation.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod assets;
mod audio_dialog;
mod browser;
mod document;
mod engine_graph;
mod settings;
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
            .with_inner_size([1280.0, 800.0]),
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
}

/// Tabs of the bottom panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum BottomTab {
    #[default]
    Editor,
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
        };
        if let Some(dir) = app.settings.browser_dir.clone() {
            app.browser.set_dir(&dir);
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
                self.output = Some((out, engine));
                self.audio_error = None;
            }
            Err(e) => {
                tracing::error!("audio output failed: {e}");
                self.audio_error = Some(e.to_string());
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
                self.set_status(format!("Could not open {}: {e}", path.display()));
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
                if ui.button("Audio settings…").clicked() {
                    self.audio_dialog.open(&self.settings.audio);
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("Keyboard shortcuts").clicked() {
                    self.show_shortcuts = true;
                }
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(err) = &self.audio_error {
                ui.colored_label(ui.visuals().error_fg_color, format!("Audio: {err}"));
            } else if let Some((out, engine)) = &self.output {
                if out.has_failed() {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        "Audio device stopped working — check Options > Audio settings",
                    );
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
                        self.graph.sent = built.info.params(&self.doc.project);
                        self.graph.info = built.info;
                    }
                    Err(e) => tracing::error!("building the engine graph failed: {e}"),
                }
            }
        }
        for (key, value) in self.graph.info.params(&self.doc.project) {
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

    fn apply_actions(&mut self, actions: Vec<UiAction>) {
        for action in actions {
            match action {
                UiAction::Edit { edit, merge } => {
                    if let Err(e) = self.doc.perform(edit, merge) {
                        self.set_status(format!("Edit failed: {e}"));
                    }
                }
                UiAction::Import { files, track, at } => self.import(files, track, at),
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
            ui.selectable_value(&mut self.bottom_tab, BottomTab::Mixer, "Mixer");
        });
        ui.separator();
        match self.bottom_tab {
            BottomTab::Editor => {
                ui.weak("Select a clip to edit it here.");
                Vec::new()
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

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
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
            let input = ArrangeInput {
                playhead,
                playing,
                peaks: self.assets.peaks(),
            };
            actions = self.arrange.ui(ui, &mut self.doc.project, &input);
        });
        self.apply_actions(actions);
        self.apply_actions(bottom_actions);
        self.finish_imports();
        self.finish_recordings();
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
        self.unsaved_dialog(&ctx);

        if let Some((_, engine)) = &mut self.output {
            self.sync.sync(&self.doc.project, engine);
        }
        match self.doc.autosave_if_due(Settings::data_dir().as_deref()) {
            Some(Ok(path)) => tracing::info!("autosaved to {}", path.display()),
            Some(Err(e)) => self.set_status(format!("Autosave failed: {e}")),
            None => {}
        }
        self.update_title(&ctx);
    }
}
