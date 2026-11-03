//! Musiton — a digital audio workstation.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio_dialog;
mod document;
mod settings;
mod transport;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use musiton_engine::EngineHandle;
use musiton_io::audio::{self, AudioOutput, OutputRequest};
use musiton_project::EXTENSION;
use tracing_subscriber::EnvFilter;

use audio_dialog::AudioDialog;
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
        };
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
        match audio::start_output(&request) {
            Ok((out, mut engine)) => {
                self.sync.init_engine(&mut engine);
                self.output = Some((out, engine));
                self.audio_error = None;
            }
            Err(e) => {
                tracing::error!("audio output failed: {e}");
                self.audio_error = Some(e.to_string());
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
                        "Audio device stopped working — check Options → Audio settings",
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
        egui::CentralPanel::default_margins().show(ui, |_ui| {});

        if let Some(new) = self.audio_dialog.show(&ctx) {
            self.apply_audio_settings(new);
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
