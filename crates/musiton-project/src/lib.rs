//! Musiton project model (the UI-side source of truth), persistence and undo/redo.

mod edit;
mod file;
mod model;

pub use edit::{Edit, EditError, UndoStack};
pub use file::{
    load, migrate, project_dir_for, save, write_file, ProjectError, CURRENT_VERSION, EXTENSION,
};
pub use model::{
    Asset, AudioClip, ChannelStrip, Clip, Marker, MidiClip, Note, OutputRoute, Project, SendRoute,
    Track, TrackKind,
};
