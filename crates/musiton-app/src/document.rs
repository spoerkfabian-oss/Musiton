//! The open project: file path, undo history, dirty state, autosave.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use musiton_project::{Edit, EditError, Project, ProjectError, UndoStack, EXTENSION};

/// How often unsaved changes are written to the autosave file.
pub const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(120);

/// A project plus everything needed to edit and save it.
pub struct Document {
    /// The song.
    pub project: Project,
    /// Where it is saved; `None` until the first "Save As".
    pub path: Option<PathBuf>,
    undo: UndoStack,
    /// Changes that are not undoable (loop on/off, metronome) since the last save.
    settings_dirty: bool,
    last_autosave: Instant,
    autosaved: Option<Project>,
    /// Bumped on every project change (triggers engine graph rebuilds).
    pub revision: u64,
}

impl Default for Document {
    fn default() -> Self {
        Self::new(Project::default(), None)
    }
}

impl Document {
    fn new(project: Project, path: Option<PathBuf>) -> Self {
        Self {
            autosaved: Some(project.clone()),
            project,
            path,
            undo: UndoStack::new(),
            settings_dirty: false,
            last_autosave: Instant::now(),
            revision: 0,
        }
    }

    /// Opens a project file.
    pub fn open(path: &Path) -> Result<Self, ProjectError> {
        Ok(Self::new(
            musiton_project::load(path)?,
            Some(path.to_path_buf()),
        ))
    }

    /// Song name for the title bar.
    pub fn name(&self) -> String {
        self.path
            .as_deref()
            .and_then(Path::file_stem)
            .map_or("Untitled".to_owned(), |s| s.to_string_lossy().into_owned())
    }

    /// `true` if there are unsaved changes.
    pub fn is_dirty(&self) -> bool {
        self.undo.is_dirty() || self.settings_dirty
    }

    /// Records a non-undoable change.
    pub fn settings_changed(&mut self) {
        self.settings_dirty = true;
    }

    /// Applies an undoable edit.
    pub fn perform(&mut self, edit: Edit, merge: bool) -> Result<(), EditError> {
        self.revision += 1;
        self.undo.perform(&mut self.project, edit, merge)
    }

    /// The project folder (where `audio/` lives), once saved.
    pub fn dir(&self) -> Option<&Path> {
        self.path.as_deref().and_then(Path::parent)
    }

    /// Undo; returns the label of what was undone.
    pub fn undo(&mut self) -> Result<Option<&'static str>, EditError> {
        self.revision += 1;
        self.undo.undo(&mut self.project)
    }

    /// Redo; returns the label of what was redone.
    pub fn redo(&mut self) -> Result<Option<&'static str>, EditError> {
        self.revision += 1;
        self.undo.redo(&mut self.project)
    }

    /// Label for the Undo menu entry.
    pub fn undo_label(&self) -> Option<&'static str> {
        self.undo.undo_label()
    }

    /// Label for the Redo menu entry.
    pub fn redo_label(&self) -> Option<&'static str> {
        self.undo.redo_label()
    }

    /// Saves to `path` (which becomes the document's path).
    pub fn save_to(&mut self, path: &Path) -> Result<(), ProjectError> {
        musiton_project::save(&self.project, path)?;
        self.path = Some(path.to_path_buf());
        self.undo.mark_saved();
        self.settings_dirty = false;
        Ok(())
    }

    /// Where the autosave copy goes: `<project folder>/autosave/<name>.musiton`, or the app
    /// data folder for an untitled song.
    pub fn autosave_path(&self, data_dir: Option<&Path>) -> Option<PathBuf> {
        let file = format!("{}.{EXTENSION}", self.name());
        match &self.path {
            Some(p) => Some(p.parent()?.join("autosave").join(file)),
            None => Some(data_dir?.join("autosave").join(file)),
        }
    }

    /// Writes the autosave copy if the interval passed and something changed.
    /// Returns the path written to.
    pub fn autosave_if_due(
        &mut self,
        data_dir: Option<&Path>,
    ) -> Option<Result<PathBuf, ProjectError>> {
        if self.last_autosave.elapsed() < AUTOSAVE_INTERVAL {
            return None;
        }
        self.last_autosave = Instant::now();
        if self.autosaved.as_ref() == Some(&self.project) {
            return None;
        }
        let path = self.autosave_path(data_dir)?;
        let result = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .map_err(ProjectError::from)
            .and_then(|()| musiton_project::write_file(&self.project, &path));
        if result.is_ok() {
            self.autosaved = Some(self.project.clone());
        }
        Some(result.map(|()| path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use musiton_core::TempoMap;

    #[test]
    fn save_clears_dirty_and_sets_name() {
        let dir = std::env::temp_dir().join(format!("musiton-doc-{}", std::process::id()));
        let path = dir.join("Song").join("Song.musiton");
        let mut doc = Document::default();
        assert_eq!(doc.name(), "Untitled");
        doc.perform(Edit::SetTempo(TempoMap::constant(90.0)), false)
            .unwrap();
        doc.settings_changed();
        assert!(doc.is_dirty());
        doc.save_to(&path).unwrap();
        assert!(!doc.is_dirty());
        assert_eq!(doc.name(), "Song");
        assert_eq!(
            doc.autosave_path(None),
            Some(dir.join("Song").join("autosave").join("Song.musiton"))
        );
        let reopened = Document::open(&path).unwrap();
        assert_eq!(reopened.project, doc.project);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn untitled_autosaves_to_data_dir() {
        let doc = Document::default();
        assert_eq!(
            doc.autosave_path(Some(Path::new("/data"))),
            Some(PathBuf::from("/data/autosave/Untitled.musiton"))
        );
        assert_eq!(doc.autosave_path(None), None);
    }
}
