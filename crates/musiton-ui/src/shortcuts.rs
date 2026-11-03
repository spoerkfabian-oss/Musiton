//! Keyboard shortcut reference.

use egui::Ui;

/// All keyboard shortcuts: (keys, action).
pub const SHORTCUTS: &[(&str, &str)] = &[
    ("Space", "Play / stop"),
    ("Home", "Return to start"),
    ("L", "Loop on/off"),
    ("Ctrl+N", "New project"),
    ("Ctrl+O", "Open project"),
    ("Ctrl+S", "Save"),
    ("Ctrl+Shift+S", "Save as"),
    ("Ctrl+Z", "Undo"),
    ("Ctrl+Y / Ctrl+Shift+Z", "Redo"),
    ("Ctrl+T", "Add audio track"),
    ("Ctrl+Shift+T", "Add instrument track"),
    ("Delete", "Delete selected track"),
    ("F2", "Rename selected track"),
    ("Ctrl+mouse wheel", "Zoom timeline"),
    ("Shift+mouse wheel", "Scroll timeline sideways"),
    ("Mouse wheel", "Scroll tracks"),
    ("Click on ruler", "Move playhead"),
];

/// Draws the shortcut table.
pub fn shortcuts_table(ui: &mut Ui) {
    egui::Grid::new("shortcuts")
        .num_columns(2)
        .striped(true)
        .spacing([24.0, 4.0])
        .show(ui, |ui| {
            for (keys, action) in SHORTCUTS {
                ui.monospace(*keys);
                ui.label(*action);
                ui.end_row();
            }
        });
}
