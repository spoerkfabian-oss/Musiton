//! Keyboard shortcut reference.

use egui::Ui;

/// All keyboard shortcuts: (keys, action).
pub const SHORTCUTS: &[(&str, &str)] = &[
    ("Space", "Play / stop"),
    ("Home", "Return to start"),
    ("L", "Loop on/off"),
    ("R", "Record on armed tracks"),
    ("A", "Automation view on/off"),
    ("Click (automation view)", "Add automation point"),
    ("Right-click point", "Delete automation point"),
    (
        "Alt+drag (automation view)",
        "Bend the curve between two points",
    ),
    ("Ctrl+N", "New project"),
    ("Ctrl+O", "Open project"),
    ("Ctrl+S", "Save"),
    ("Ctrl+Shift+S", "Save as"),
    ("Ctrl+Z", "Undo"),
    ("Ctrl+Y / Ctrl+Shift+Z", "Redo"),
    ("Ctrl+T", "Add audio track"),
    ("Ctrl+Shift+T", "Add instrument track"),
    ("Delete", "Delete selected clips (or the selected track)"),
    ("Ctrl+D", "Duplicate selected clips"),
    ("S", "Split selected clips at the playhead"),
    ("Shift/Ctrl+click", "Add a clip to the selection"),
    (
        "Drag on empty lane space",
        "Select clips in a rectangle (Shift/Ctrl: add)",
    ),
    ("Drag clip", "Move (also to another track of the same kind)"),
    ("Drag clip edge", "Trim"),
    ("Drag white handle", "Fade in / fade out"),
    ("Right-click clip", "Clip gain"),
    ("F2", "Rename selected track"),
    ("Ctrl+mouse wheel", "Zoom timeline"),
    ("Shift+mouse wheel", "Scroll timeline sideways"),
    ("Mouse wheel", "Scroll tracks"),
    ("Click on ruler", "Move playhead"),
    ("Piano roll: Ctrl+A", "Select all notes"),
    ("Piano roll: Delete", "Delete selected notes"),
    ("Piano roll: Ctrl+D", "Duplicate selected notes"),
    ("Piano roll: Q", "Quantize notes to the grid"),
    (
        "Piano roll: Up / Down",
        "Transpose selected notes by a semitone",
    ),
    (
        "Piano roll: Shift+Up / Shift+Down",
        "Transpose selected notes by an octave",
    ),
    ("Piano roll: click (Draw tool)", "Add a note"),
    ("Piano roll: right-click note", "Delete note"),
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
