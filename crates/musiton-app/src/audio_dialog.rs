//! "Audio settings" window: output/input devices, sample rate, buffer size, recording latency.

use eframe::egui;
use musiton_io::audio::{self, InputDeviceInfo, OutputDeviceInfo};

use crate::settings::AudioSettings;

/// State of the audio settings window.
#[derive(Default)]
pub struct AudioDialog {
    /// Whether the window is shown.
    pub open: bool,
    devices: Vec<OutputDeviceInfo>,
    inputs: Vec<InputDeviceInfo>,
    list_error: Option<String>,
    draft: AudioSettings,
    /// Status text of the latency measurement, set by the app.
    pub measure_status: String,
}

/// What the user asked for in the dialog.
pub enum DialogAction {
    /// Use these settings (restarts audio).
    Apply(AudioSettings),
    /// Measure the round-trip latency now.
    Measure,
    /// Only the recording offset changed (no audio restart needed).
    Offset(f32),
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
        self.inputs = audio::list_input_devices().unwrap_or_default();
    }

    /// Draws the window.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<DialogAction> {
        let mut open = self.open;
        let mut action = None;
        egui::Window::new("Audio settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| action = self.contents(ui));
        self.open = open && !matches!(action, Some(DialogAction::Apply(_)));
        action
    }

    /// Stores a measured latency in the draft (so a later Apply keeps it).
    pub fn set_measured(&mut self, frames: Option<u32>) {
        self.draft.measured_latency = frames;
    }

    fn contents(&mut self, ui: &mut egui::Ui) -> Option<DialogAction> {
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

                ui.label("Input device");
                let input_text = if !self.draft.input_enabled {
                    "None".to_owned()
                } else {
                    match &self.draft.input_device {
                        None => "System default".to_owned(),
                        Some(id) => self
                            .inputs
                            .iter()
                            .find(|d| &d.id == id)
                            .map_or(format!("{id} (not connected)"), |d| d.name.clone()),
                    }
                };
                egui::ComboBox::from_id_salt("input")
                    .selected_text(input_text)
                    .width(320.0)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(!self.draft.input_enabled, "None")
                            .clicked()
                        {
                            self.draft.input_enabled = false;
                        }
                        let default = self.draft.input_enabled && self.draft.input_device.is_none();
                        if ui.selectable_label(default, "System default").clicked() {
                            self.draft.input_enabled = true;
                            self.draft.input_device = None;
                        }
                        for d in &self.inputs {
                            let sel = self.draft.input_enabled
                                && self.draft.input_device.as_deref() == Some(d.id.as_str());
                            let label = format!("{} ({} ch)", d.name, d.channels);
                            if ui.selectable_label(sel, label).clicked() {
                                self.draft.input_enabled = true;
                                self.draft.input_device = Some(d.id.clone());
                            }
                        }
                    });
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
        let mut action = None;
        ui.strong("Recording latency");
        ui.horizontal(|ui| {
            match self.draft.measured_latency {
                Some(f) => ui.label(format!("Measured: {f} samples")),
                None => ui.label("Not measured (estimated from the buffer sizes)"),
            };
            if ui
                .button("Measure")
                .on_hover_text(
                    "Connect an output to input 1 with a cable (or hold a microphone to the \
                     speaker), then click. A short click is played and timed.",
                )
                .clicked()
            {
                action = Some(DialogAction::Measure);
            }
            if self.draft.measured_latency.is_some() && ui.small_button("forget").clicked() {
                self.draft.measured_latency = None;
            }
        });
        if !self.measure_status.is_empty() {
            ui.weak(&self.measure_status);
        }
        ui.horizontal(|ui| {
            ui.label("Manual correction");
            let r = ui
                .add(
                    egui::DragValue::new(&mut self.draft.record_offset_ms)
                        .range(-200.0..=200.0)
                        .speed(0.1)
                        .suffix(" ms"),
                )
                .on_hover_text("Positive values move recordings earlier");
            if r.changed() {
                action = Some(DialogAction::Offset(self.draft.record_offset_ms));
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Refresh devices").clicked() {
                self.refresh();
            }
            if ui.button("Apply").clicked() {
                action = Some(DialogAction::Apply(self.draft.clone()));
            }
        });
        action
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
