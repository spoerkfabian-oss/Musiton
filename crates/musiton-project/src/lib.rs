//! Musiton project model (the UI-side source of truth), persistence and undo/redo.

mod edit;
mod file;
mod model;

pub use edit::{Edit, EditError, UndoStack};
pub use file::{
    load, migrate, project_dir_for, save, write_file, ProjectError, CURRENT_VERSION, EXTENSION,
};
pub use model::{
    Asset, AudioClip, AutoMode, AutoPoint, AutomationLane, ChannelStrip, Clip, DeviceKind,
    DeviceSlot, InputRoute, Marker, MidiClip, MonitorMode, Note, OutputRoute, ParamTarget, Project,
    RecordSetup, SampleZone, SendRoute, Track, TrackKind, PAN_PARAM, VOLUME_PARAM,
};
