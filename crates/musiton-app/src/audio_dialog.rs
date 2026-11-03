//! "Audio settings" window: choose output device, sample rate and buffer size.

use eframe::egui;
use musiton_io::audio::{self, OutputDeviceInfo};

use crate::settings::AudioSettings;

/// State of the audio settings window.
#[derive(Default)]
pub struct AudioDialog {
    /// Whether the window is shown.
    pub open: bool,
    devices: Vec<OutputDeviceInfo>,
    list_error: Option<String>,
    draft: AudioSettings,
}

impl AudioDialog {
    /// Opens the window with `current` as the starting selection.
    pub fn open(&mut self, current: &AudioSettings) {
        self.open = true;
        self.draft = current.clone();
        self.refresh();
    }

    fn refresh(&mut self) {
        match audio::list_output_devices() {
            Ok(d) => {
                self.devices = d;
                self.list_error = None;
            }
            Err(e) => self.list_error = Some(e.to_string()),
        }
    }

    /// Draws the window. Returns new settings when the user clicks "Apply".
    pub fn show(&mut self, ctx: &egui::Context) -> Option<AudioSettings> {
        let mut open = self.open;
        let mut applied = None;
        egui::Window::new("Audio settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| applied = self.contents(ui));
        self.open = open && applied.is_none();
        applied
    }

    fn contents(&mut self, ui: &mut egui::Ui) -> Option<AudioSettings> {
        if let Some(err) = &self.list_error {
            ui.colored_label(ui.visuals().error_fg_color, err);
        }
        let selected = self
            .draft
            .output_device
            .as_ref()
            .and_then(|id| self.devices.iter().find(|d| &d.id == id));
        let default_dev = self.devices.iter().find(|d| d.is_default);
        let caps = selected.or(default_dev);
        let rates = caps.map(|d| d.sample_rates.clone()).unwrap_or_default();
        let sizes = caps
            .map(|d| d.buffer_sizes.clone())
            .unwrap_or_else(|| audio::STANDARD_BUFFER_SIZES.to_vec());

        egui::Grid::new("audio_grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Output device");
                let text = match (&self.draft.output_device, selected) {
                    (None, _) => "System default".to_owned(),
                    (Some(_), Some(d)) => d.name.clone(),
                    (Some(id), None) => format!("{id} (not connected)"),
                };
                egui::ComboBox::from_id_salt("device")
                    .selected_text(text)
                    .width(320.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.draft.output_device, None, "System default");
                        for d in &self.devices {
                            ui.selectable_value(
                                &mut self.draft.output_device,
                                Some(d.id.clone()),
                                &d.name,
                            );
                        }
                    });
                ui.end_row();

                ui.label("Sample rate");
                combo(ui, "rate", &mut self.draft.sample_rate, &rates, "Hz");
                ui.end_row();

                ui.label("Buffer size");
                combo(ui, "buffer", &mut self.draft.buffer_size, &sizes, "samples");
                ui.end_row();
            });
        if let Some(size) = self.draft.buffer_size {
            let rate = self.draft.sample_rate.unwrap_or(48_000);
            ui.weak(format!(
                "Latency per buffer: {:.1} ms",
                size as f32 * 1000.0 / rate as f32
            ));
        }
        ui.separator();
        let mut applied = None;
        ui.horizontal(|ui| {
            if ui.button("Refresh devices").clicked() {
                self.refresh();
            }
            if ui.button("Apply").clicked() {
                applied = Some(self.draft.clone());
            }
        });
        applied
    }
}

fn combo(ui: &mut egui::Ui, id: &str, value: &mut Option<u32>, options: &[u32], unit: &str) {
    let label = |v: Option<u32>| v.map_or("Device default".to_owned(), |v| format!("{v} {unit}"));
    egui::ComboBox::from_id_salt(id)
        .selected_text(label(*value))
        .show_ui(ui, |ui| {
            ui.selectable_value(value, None, label(None));
            for &o in options {
                ui.selectable_value(value, Some(o), label(Some(o)));
            }
        });
}
