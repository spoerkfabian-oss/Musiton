//! egui widgets for Musiton. This crate never talks to the audio engine: widgets return
//! [`UiAction`]s that the app applies (edits go through the undo stack).

pub mod arrange;
pub mod clip_edit;
pub mod clip_view;
pub mod device_panel;
pub mod mixer;
pub mod note_edit;
pub mod piano_roll;
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
    /// Play (or stop) a note on the live-MIDI track to hear it.
    PreviewNote {
        /// Start (true) or stop.
        on: bool,
        /// Key 0..127.
        key: u8,
    },
    /// Replace a track's device chain; the app allocates ids for new devices.
    NewDevice {
        /// Which track.
        track: musiton_core::ids::TrackId,
        /// The new chain.
        devices: Vec<musiton_project::DeviceSlot>,
    },
    /// Ask the user for a sample file and put it into the track's sampler.
    LoadSample {
        /// Which track.
        track: musiton_core::ids::TrackId,
        /// Pad key in drum mode; `None` = the melodic sample.
        key: Option<u8>,
    },
    /// Save the current settings as a user preset.
    SavePreset {
        /// Device type.
        type_id: String,
        /// Preset name.
        name: String,
        /// Parameter values.
        params: Vec<(u64, f32)>,
    },
    /// Open a MIDI clip in the piano roll.
    OpenEditor {
        /// Track of the clip.
        track: musiton_core::ids::TrackId,
        /// The clip.
        clip: musiton_core::ids::ClipId,
    },
    /// Import audio files as clips.
    Import {
        /// Files to import.
        files: Vec<std::path::PathBuf>,
        /// Track to put them on; `None` = new track(s).
        track: Option<musiton_core::ids::TrackId>,
        /// Start position.
        at: Tick,
    },
}

/// `true` while a text field (or a number field in typing mode) has keyboard focus,
/// so single-key shortcuts must be ignored.
pub fn text_input_focused(ctx: &egui::Context) -> bool {
    ctx.memory(|m| m.focused())
        .is_some_and(|id| egui::TextEdit::load_state(ctx, id).is_some())
}
