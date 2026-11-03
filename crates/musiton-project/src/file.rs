//! Project files: `MySong/MySong.musiton` (RON text) plus `audio/`, `peaks/`, `plugins/`
//! and `autosave/` folders next to it.

use std::path::{Path, PathBuf};

use crate::Project;

/// File format version written by this build.
pub const CURRENT_VERSION: u32 = 1;
/// Project file extension (without dot).
pub const EXTENSION: &str = "musiton";
/// Sub-folders created next to the project file.
const SUBFOLDERS: [&str; 4] = ["audio", "peaks", "plugins", "autosave"];

/// Errors when loading or saving a project.
#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    /// File system error.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// The file is not a valid project.
    #[error("the project file is damaged: {0}")]
    Parse(String),
    /// Written by a newer Musiton.
    #[error("this project was made with a newer Musiton (format {0}); please update")]
    TooNew(u32),
}

/// Where Save As should put a project the user called `chosen` (e.g. `C:\Music\MySong.musiton`):
/// a folder named after the song is created unless the chosen folder already has that name.
pub fn project_dir_for(chosen: &Path) -> PathBuf {
    let stem = chosen
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled".to_owned());
    let parent = chosen.parent().unwrap_or(Path::new("."));
    let file = format!("{stem}.{EXTENSION}");
    if parent
        .file_name()
        .is_some_and(|n| n.to_string_lossy() == stem)
    {
        parent.join(file)
    } else {
        parent.join(&stem).join(file)
    }
}

/// Saves `project` to `path` and creates the project sub-folders next to it.
pub fn save(project: &Project, path: &Path) -> Result<(), ProjectError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    for sub in SUBFOLDERS {
        std::fs::create_dir_all(dir.join(sub))?;
    }
    write_file(project, path)
}

/// Writes only the project file, atomically (temp file + rename). Used for autosave.
pub fn write_file(project: &Project, path: &Path) -> Result<(), ProjectError> {
    let text = ron::ser::to_string_pretty(project, ron::ser::PrettyConfig::default())
        .map_err(|e| ProjectError::Parse(e.to_string()))?;
    let tmp = path.with_extension(format!("{EXTENSION}.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Reads a project, upgrading older formats via [`migrate`].
pub fn load(path: &Path) -> Result<Project, ProjectError> {
    let text = std::fs::read_to_string(path)?;
    let header: Header = ron::from_str(&text).map_err(|e| ProjectError::Parse(e.to_string()))?;
    if header.version > CURRENT_VERSION {
        return Err(ProjectError::TooNew(header.version));
    }
    let text = migrate(text, header.version)?;
    let mut project: Project =
        ron::from_str(&text).map_err(|e| ProjectError::Parse(e.to_string()))?;
    project.version = CURRENT_VERSION;
    Ok(project)
}

/// Just the version field of a project file (other fields are ignored).
#[derive(serde::Deserialize)]
#[serde(rename = "Project")]
struct Header {
    #[serde(default = "first_version")]
    version: u32,
}

fn first_version() -> u32 {
    1
}

/// Upgrades the text of a project written with format `from` to [`CURRENT_VERSION`].
/// Each format change adds one step here that converts version N to N + 1
/// (typically: parse into the old structs, convert, write again).
pub fn migrate(text: String, from: u32) -> Result<String, ProjectError> {
    let mut version = from.max(1);
    while version < CURRENT_VERSION {
        version += 1;
    }
    Ok(text)
}
