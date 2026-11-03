//! Musiton — a digital audio workstation.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tracing_subscriber::EnvFilter;

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
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(APP_NAME, options, Box::new(|_cc| Ok(Box::new(App))))
        .map_err(|e| anyhow::anyhow!("failed to start UI: {e}"))
}

/// Top-level application state.
struct App;

impl eframe::App for App {
    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        eframe::egui::CentralPanel::default_margins().show(ui, |_ui| {});
    }
}
