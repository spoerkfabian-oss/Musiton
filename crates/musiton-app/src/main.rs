//! Musiton — a digital audio workstation.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio_dialog;
mod settings;
mod transport;

use std::path::PathBuf;
use std::time::Duration;

use eframe::egui;
use musiton_engine::EngineHandle;
use musiton_io::audio::{self, AudioOutput, OutputRequest};
use tracing_subscriber::EnvFilter;

use audio_dialog::AudioDialog;
use settings::{AudioSettings, Settings};
use transport::Transport;

/// In debug builds every allocation inside `musiton_engine::rt::no_alloc` aborts.
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: musiton_engine::rt::AllocDisabler = musiton_engine::rt::AllocDisabler;

/// Application name shown in the window title.
const APP_NAME: &str = "Musiton";

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,wgpu=warn")),
        )
        .init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting {APP_NAME}");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(APP_NAME, options, Box::new(|_cc| Ok(Box::new(App::new()))))
        .map_err(|e| anyhow::anyhow!("failed to start UI: {e}"))
}

/// Top-level application state (UI thread).
struct App {
    settings: Settings,
    settings_path: Option<PathBuf>,
    output: Option<(AudioOutput, EngineHandle)>,
    transport: Transport,
    audio_error: Option<String>,
    audio_dialog: AudioDialog,
}

impl App {
    fn new() -> Self {
        let settings_path = Settings::default_path();
        let settings = settings_path
            .as_deref()
            .map(Settings::load_from)
            .unwrap_or_default();
        let mut app = Self {
            settings,
            settings_path,
            output: None,
            transport: Transport::default(),
            audio_error: None,
            audio_dialog: AudioDialog::default(),
        };
        app.restart_audio();
        app
    }

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
                self.transport.init_engine(&mut engine);
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
        if let Some(path) = &self.settings_path {
            if let Err(e) = self.settings.save_to(path) {
                tracing::error!("saving settings failed: {e}");
            }
        }
        self.restart_audio();
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
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
                return;
            }
            let Some((out, engine)) = &self.output else {
                ui.label("Audio: off");
                return;
            };
            if out.has_failed() {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    "Audio device stopped working — check Options → Audio settings",
                );
                return;
            }
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
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.ctx().request_repaint_after(Duration::from_millis(100));

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::top("transport").show(ui, |ui| {
            let engine = self.output.as_mut().map(|(_, e)| e);
            self.transport.ui(ui, engine);
        });
        egui::CentralPanel::default_margins().show(ui, |_ui| {});

        if let Some(new) = self.audio_dialog.show(ui.ctx()) {
            self.apply_audio_settings(new);
        }
    }
}
