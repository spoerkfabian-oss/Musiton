//! File browser panel: navigate folders, drag audio files into the arrangement.

use std::path::{Path, PathBuf};

use eframe::egui;
use musiton_io::file::is_audio_file;

/// Browser state.
#[derive(Default)]
pub struct Browser {
    dir: Option<PathBuf>,
    entries: Vec<(PathBuf, bool)>,
}

/// What the user did in the browser.
pub enum BrowserAction {
    /// A different folder is shown (remember it in the settings).
    FolderChanged(PathBuf),
    /// Double-clicked an audio file.
    Import(PathBuf),
}

impl Browser {
    /// Shows `dir`.
    pub fn set_dir(&mut self, dir: &Path) {
        let mut entries: Vec<(PathBuf, bool)> = std::fs::read_dir(dir)
            .map(|it| {
                it.filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| {
                        let hidden = p
                            .file_name()
                            .is_some_and(|n| n.to_string_lossy().starts_with('.'));
                        !hidden && (p.is_dir() || is_audio_file(p))
                    })
                    .map(|p| {
                        let is_dir = p.is_dir();
                        (p, is_dir)
                    })
                    .collect()
            })
            .unwrap_or_default();
        entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        self.entries = entries;
        self.dir = Some(dir.to_path_buf());
    }

    /// Draws the panel.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<BrowserAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            ui.strong("Browser");
            if ui.button("Folder…").clicked() {
                let mut d = rfd::FileDialog::new().set_title("Choose a folder with audio files");
                if let Some(dir) = &self.dir {
                    d = d.set_directory(dir);
                }
                if let Some(dir) = d.pick_folder() {
                    self.set_dir(&dir);
                    action = Some(BrowserAction::FolderChanged(dir));
                }
            }
        });
        ui.separator();
        let Some(dir) = self.dir.clone() else {
            ui.weak("Choose a folder to see its audio files. Drag files into the arrangement.");
            return action;
        };
        ui.horizontal(|ui| {
            if let Some(parent) = dir.parent() {
                if ui
                    .small_button("↑")
                    .on_hover_text("Parent folder")
                    .clicked()
                {
                    let parent = parent.to_path_buf();
                    self.set_dir(&parent);
                    action = Some(BrowserAction::FolderChanged(parent));
                }
            }
            ui.add(egui::Label::new(egui::RichText::new(name(&dir)).weak()).truncate());
        });
        egui::ScrollArea::vertical().show(ui, |ui| {
            let mut go = None;
            for (path, is_dir) in &self.entries {
                if *is_dir {
                    if ui
                        .selectable_label(false, format!("📁 {}", name(path)))
                        .double_clicked()
                    {
                        go = Some(path.clone());
                    }
                } else {
                    let id = egui::Id::unique(("browser-file", path));
                    let r = ui
                        .dnd_drag_source(id, path.clone(), |ui| {
                            ui.add(egui::Label::new(name(path)).truncate().selectable(false));
                        })
                        .response;
                    if r.double_clicked() {
                        action = Some(BrowserAction::Import(path.clone()));
                    }
                    r.on_hover_text("Drag into a track, or double-click to add on a new track");
                }
            }
            if let Some(p) = go {
                self.set_dir(&p);
                action = Some(BrowserAction::FolderChanged(p));
            }
        });
        action
    }
}

fn name(p: &Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}
