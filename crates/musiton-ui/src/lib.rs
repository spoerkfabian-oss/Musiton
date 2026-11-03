//! egui widgets for Musiton. This crate never talks to the audio engine: widgets return
//! [`UiAction`]s that the app applies (edits go through the undo stack).

pub mod arrange;
pub mod shortcuts;
pub mod snap;
pub mod view;

use musiton_core::Tick;
use musiton_project::Edit;

/// Something a widget asks the app to do.
#[derive(Debug)]
pub enum UiAction {
    /// Apply an undoable edit; `merge` joins it with the previous edit of the same value.
    Edit {
        /// The edit.
        edit: Edit,
        /// Merge with the previous edit (continuous drag).
        merge: bool,
    },
    /// Move the playhead.
    Locate(Tick),
}

/// `true` while a text field (or a number field in typing mode) has keyboard focus,
/// so single-key shortcuts must be ignored.
pub fn text_input_focused(ctx: &egui::Context) -> bool {
    ctx.memory(|m| m.focused())
        .is_some_and(|id| egui::TextEdit::load_state(ctx, id).is_some())
}
