//! "Missing audio files" dialog: locate files one by one or search a folder for them.

use std::path::{Path, PathBuf};

use eframe::egui;
use musiton_core::ids::AssetId;

/// What the user asked for.
#[derive(Debug, Clone, PartialEq)]
pub enum RelinkAction {
    /// Use `file` for asset `id`.
    Use(AssetId, PathBuf),
    /// Close the dialog (files stay missing).
    Close,
}

/// The dialog state: the missing files (asset id, path relative to the project).
#[derive(Debug, Default)]
pub struct RelinkDialog {
    /// Files that could not be loaded.
    pub missing: Vec<(AssetId, String)>,
    /// Result of the last folder search.
    note: String,
}

impl RelinkDialog {
    /// Shows the dialog while files are missing.
    pub fn show(&mut self, ctx: &egui::Context) -> Vec<RelinkAction> {
        let mut out = Vec::new();
        if self.missing.is_empty() {
            return out;
        }
        egui::Modal::new(egui::Id::unique("relink")).show(ctx, |ui| {
            ui.heading("Missing audio files");
            ui.label(format!(
                "{} audio file(s) of this song could not be found. Their clips stay silent \
                 until you show Musiton where the files are.",
                self.missing.len()
            ));
            ui.add_space(6.0);
            egui::ScrollArea::vertical()
                .max_height(240.0)
                .show(ui, |ui| {
                    for (id, file) in &self.missing {
                        ui.horizontal(|ui| {
                            if ui.button("Locate…").clicked() {
                                if let Some(p) = pick_file(file) {
                                    out.push(RelinkAction::Use(*id, p));
                                }
                            }
                            ui.label(file);
                        });
                    }
                });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui
                    .button("Search folder…")
                    .on_hover_text("Finds files with the same names in a folder and its subfolders")
                    .clicked()
                {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        let found = search(&dir, &self.missing);
                        self.note = format!("Found {} of {}.", found.len(), self.missing.len());
                        out.extend(found.into_iter().map(|(id, p)| RelinkAction::Use(id, p)));
                    }
                }
                if ui.button("Close").clicked() {
                    out.push(RelinkAction::Close);
                }
            });
            if !self.note.is_empty() {
                ui.weak(&self.note);
            }
        });
        if out.contains(&RelinkAction::Close) {
            self.missing.clear();
            self.note.clear();
        }
        out
    }
}

fn pick_file(missing: &str) -> Option<PathBuf> {
    let name = Path::new(missing)
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    rfd::FileDialog::new()
        .set_title(format!("Locate {name}"))
        .add_filter("Audio", &musiton_io::file::AUDIO_EXTENSIONS)
        .pick_file()
}

/// Deepest folder level searched below the chosen folder.
const MAX_DEPTH: usize = 6;

/// Looks for files named like the missing ones below `dir`.
pub fn search(dir: &Path, missing: &[(AssetId, String)]) -> Vec<(AssetId, PathBuf)> {
    let mut found = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for path in entries.flatten().map(|e| e.path()) {
            if path.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            let Some(name) = path.file_name() else {
                continue;
            };
            for (id, file) in missing {
                let wanted = Path::new(file).file_name();
                let unseen = !found.iter().any(|(f, _)| f == id);
                if unseen && wanted.is_some_and(|w| w.eq_ignore_ascii_case(name)) {
                    found.push((*id, path.clone()));
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_search_finds_files_by_name_in_subfolders() {
        let dir = std::env::temp_dir().join(format!("musiton-relink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::write(dir.join("a/b/Kick.wav"), b"x").unwrap();
        std::fs::write(dir.join("snare.wav"), b"x").unwrap();
        let missing = vec![
            (AssetId(1), "audio/kick.wav".to_owned()),
            (AssetId(2), "audio/snare.wav".to_owned()),
            (AssetId(3), "audio/gone.wav".to_owned()),
        ];
        let mut found = search(&dir, &missing);
        found.sort_by_key(|(id, _)| id.0);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0], (AssetId(1), dir.join("a/b/Kick.wav")));
        assert_eq!(found[1], (AssetId(2), dir.join("snare.wav")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
