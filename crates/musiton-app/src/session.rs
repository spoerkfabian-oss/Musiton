//! Crash detection: a lock file exists while Musiton runs; if it is still there at the
//! next start, the previous session ended unexpectedly and its autosave can be recovered.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Contents of the lock file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionLock {
    /// The project that was open (`None` = an unsaved song).
    pub project: Option<PathBuf>,
}

/// A recovery the user can accept.
#[derive(Debug, Clone, PartialEq)]
pub struct Recovery {
    /// The autosave file to load.
    pub autosave: PathBuf,
    /// The project it belongs to.
    pub project: Option<PathBuf>,
    /// When the autosave was written.
    pub modified: std::time::SystemTime,
}

/// Lock file location inside `data_dir`.
pub fn lock_path(data_dir: &Path) -> PathBuf {
    data_dir.join("session.lock")
}

/// Records that `project` is open (creates/updates the lock file).
pub fn write_lock(data_dir: &Path, project: Option<&Path>) {
    let lock = SessionLock {
        project: project.map(Path::to_path_buf),
    };
    let _ = std::fs::create_dir_all(data_dir);
    if let Ok(text) = serde_json::to_string(&lock) {
        let _ = std::fs::write(lock_path(data_dir), text);
    }
}

/// Removes the lock file (clean exit).
pub fn clear_lock(data_dir: &Path) {
    let _ = std::fs::remove_file(lock_path(data_dir));
}

/// If the last session crashed and left a newer autosave, returns it.
/// `autosave_for` gives the autosave path of a project (or of an unsaved song).
pub fn find_recovery(
    data_dir: &Path,
    autosave_for: &dyn Fn(Option<&Path>) -> Option<PathBuf>,
) -> Option<Recovery> {
    let text = std::fs::read_to_string(lock_path(data_dir)).ok()?;
    let lock: SessionLock = serde_json::from_str(&text).ok()?;
    let autosave = autosave_for(lock.project.as_deref())?;
    let modified = std::fs::metadata(&autosave)
        .and_then(|m| m.modified())
        .ok()?;
    let project_time = lock
        .project
        .as_ref()
        .and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    // Only worth offering if the autosave is newer than the saved project.
    if project_time.is_some_and(|t| t >= modified) {
        return None;
    }
    Some(Recovery {
        autosave,
        project: lock.project,
        modified,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_leaves_a_recoverable_autosave() {
        let dir = std::env::temp_dir().join(format!("musiton-session-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let auto = dir.join("auto.musiton");
        let finder = |_: Option<&Path>| Some(auto.clone());
        // No lock: clean exit, nothing to recover.
        assert!(find_recovery(&dir, &finder).is_none());
        write_lock(&dir, None);
        // Lock but no autosave file.
        assert!(find_recovery(&dir, &finder).is_none());
        std::fs::write(&auto, "(version: 1)").unwrap();
        let r = find_recovery(&dir, &finder).unwrap();
        assert_eq!(r.autosave, auto);
        assert_eq!(r.project, None);
        // A project saved after the autosave needs no recovery.
        let project = dir.join("song.musiton");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&project, "(version: 1)").unwrap();
        write_lock(&dir, Some(&project));
        assert!(find_recovery(&dir, &finder).is_none());
        clear_lock(&dir);
        assert!(!lock_path(&dir).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
