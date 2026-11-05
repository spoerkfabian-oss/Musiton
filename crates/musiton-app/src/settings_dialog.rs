//! Options → Settings: theme, interface size, autosave.

use eframe::egui;

use crate::settings::{Theme, UiSettings};

/// What the settings window asks for.
pub enum SettingsAction {
    /// The settings changed (apply and save).
    Changed(UiSettings),
    /// Open the audio settings.
    OpenAudio,
}

/// Settings window state.
#[derive(Default)]
pub struct SettingsDialog {
    /// Window visible.
    pub open: bool,
}

/// Applies theme and zoom to the UI.
pub fn apply(ctx: &egui::Context, ui: &UiSettings) {
    ctx.set_theme(match ui.theme {
        Theme::Dark => egui::ThemePreference::Dark,
        Theme::Light => egui::ThemePreference::Light,
        Theme::System => egui::ThemePreference::System,
    });
    ctx.set_zoom_factor(ui.scale.clamp(0.5, 3.0));
}

impl SettingsDialog {
    /// Draws the window with `current` settings.
    pub fn show(&mut self, ctx: &egui::Context, current: &UiSettings) -> Option<SettingsAction> {
        let mut action = None;
        let mut s = current.clone();
        let mut open = self.open;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                egui::Grid::new("settings_grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Theme");
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut s.theme, Theme::Dark, "Dark");
                            ui.selectable_value(&mut s.theme, Theme::Light, "Light");
                            ui.selectable_value(&mut s.theme, Theme::System, "System");
                        });
                        ui.end_row();
                        ui.label("Interface size");
                        ui.add(
                            egui::Slider::new(&mut s.scale, 0.75..=2.0)
                                .step_by(0.05)
                                .custom_formatter(|v, _| format!("{:.0} %", v * 100.0)),
                        )
                        .on_hover_text("Ctrl+Plus / Ctrl+Minus also zoom");
                        ui.end_row();
                        ui.label("Autosave");
                        ui.add(
                            egui::DragValue::new(&mut s.autosave_minutes)
                                .range(0..=60)
                                .suffix(" min"),
                        )
                        .on_hover_text("0 = off. Autosaves are used to recover after a crash.");
                        ui.end_row();
                    });
                ui.separator();
                if ui.button("Audio and MIDI devices…").clicked() {
                    action = Some(SettingsAction::OpenAudio);
                }
            });
        self.open = open;
        if s != *current && action.is_none() {
            action = Some(SettingsAction::Changed(s));
        }
        action
    }
}
