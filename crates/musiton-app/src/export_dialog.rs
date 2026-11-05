//! "Export audio" window and the progress display of a running export.

use eframe::egui;
use musiton_io::export::{ExportFormat, ExportOptions};

use crate::export::ExportRange;

/// Export settings chosen by the user.
pub struct ExportDialog {
    /// Window visible.
    pub open: bool,
    /// Range.
    pub range: ExportRange,
    /// Format.
    pub format: ExportFormat,
    /// Normalize to −0.1 dBFS.
    pub normalize: bool,
    /// Dither when exporting 16-bit.
    pub dither: bool,
    /// Tail in seconds.
    pub tail: f32,
    /// One file per track.
    pub stems: bool,
}

impl Default for ExportDialog {
    fn default() -> Self {
        Self {
            open: false,
            range: ExportRange::Song,
            format: ExportFormat::Wav24,
            normalize: false,
            dither: true,
            tail: 2.0,
            stems: false,
        }
    }
}

impl ExportDialog {
    /// Options for the writer.
    pub fn options(&self) -> ExportOptions {
        ExportOptions {
            format: self.format,
            normalize: self.normalize,
            dither: self.dither && self.format.bits() == Some(16),
        }
    }

    /// Draws the window; `true` when "Export…" was clicked.
    pub fn show(&mut self, ctx: &egui::Context, has_selection: bool, has_loop: bool) -> bool {
        let mut go = false;
        let mut open = self.open;
        egui::Window::new("Export audio")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                egui::Grid::new("export_grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Range");
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut self.range, ExportRange::Song, "Whole song");
                            ui.add_enabled_ui(has_loop, |ui| {
                                ui.selectable_value(&mut self.range, ExportRange::Loop, "Loop");
                            });
                            ui.add_enabled_ui(has_selection, |ui| {
                                ui.selectable_value(
                                    &mut self.range,
                                    ExportRange::Selection,
                                    "Selected clips",
                                );
                            });
                        });
                        ui.end_row();
                        ui.label("Format");
                        egui::ComboBox::from_id_salt("export_format")
                            .selected_text(
                                ExportFormat::ALL
                                    .iter()
                                    .find(|f| f.0 == self.format)
                                    .map_or("", |f| f.1),
                            )
                            .show_ui(ui, |ui| {
                                for (f, name) in ExportFormat::ALL {
                                    ui.selectable_value(&mut self.format, f, name);
                                }
                            });
                        ui.end_row();
                        ui.label("Tail");
                        ui.add(
                            egui::DragValue::new(&mut self.tail)
                                .range(0.0..=20.0)
                                .speed(0.1)
                                .suffix(" s"),
                        )
                        .on_hover_text("Extra time after the end for reverb and delay tails");
                        ui.end_row();
                        ui.label("Options");
                        ui.vertical(|ui| {
                            ui.checkbox(&mut self.normalize, "Normalize to −0.1 dB");
                            ui.add_enabled(
                                self.format.bits() == Some(16),
                                egui::Checkbox::new(&mut self.dither, "Dither (16-bit)"),
                            );
                            ui.checkbox(&mut self.stems, "Stems: one file per track");
                        });
                        ui.end_row();
                    });
                ui.separator();
                if ui
                    .button(if self.stems {
                        "Export stems to folder…"
                    } else {
                        "Export…"
                    })
                    .clicked()
                {
                    go = true;
                }
            });
        self.open = open && !go;
        if self.range == ExportRange::Selection && !has_selection
            || self.range == ExportRange::Loop && !has_loop
        {
            self.range = ExportRange::Song;
        }
        go
    }
}
