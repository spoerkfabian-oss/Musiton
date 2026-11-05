//! Crash detection. Every running Musiton holds an exclusively locked file in
//! `<data dir>/sessions/`. The operating system releases the lock when the process ends,
//! however it ends; a session file that can be locked at start-up therefore belongs to a
//! session that did not close properly, and its autosave can be recovered. Files of other
//! Musiton windows that are still running stay locked and are left alone.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What a session file records.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct SessionInfo {
    /// The project that was open (`None` = an unsaved song).
    project: Option<PathBuf>,
}

/// A recovery the user can accept.
#[derive(Debug, Clone, PartialEq)]
pub struct Recovery {
    /// The autosave file to load.
    pub autosave: PathBuf,
    /// The project it belongs to.
    pub project: Option<PathBuf>,
}

fn sessions_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("sessions")
}

/// The lock file of this running session.
#[derive(Debug)]
pub struct SessionLock {
    file: File,
    path: PathBuf,
    project: Option<Option<PathBuf>>,
}

impl SessionLock {
    /// Creates and locks this session's file.
    pub fn acquire(data_dir: &Path) -> Option<Self> {
        let dir = sessions_dir(data_dir);
        std::fs::create_dir_all(&dir).ok()?;
        let path = (0u32..)
            .map(|i| dir.join(format!("{}-{i}.lock", std::process::id())))
            .find(|p| !p.exists())?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .ok()?;
        file.try_lock().ok()?;
        Some(Self {
            file,
            path,
            project: None,
        })
    }

    /// Records which project is open (only writes when it changed).
    pub fn set_project(&mut self, project: Option<&Path>) {
        let project = project.map(Path::to_path_buf);
        if self.project.as_ref() == Some(&project) {
            return;
        }
        if let Ok(text) = serde_json::to_string(&SessionInfo {
            project: project.clone(),
        }) {
            let ok = self.file.set_len(0).is_ok()
                && self.file.rewind().is_ok()
                && self.file.write_all(text.as_bytes()).is_ok();
            if ok {
                self.project = Some(project);
            }
        }
    }

    /// Clean exit: unlocks and deletes the file.
    pub fn release(self) {
        let Self { file, path, .. } = self;
        drop(file);
        let _ = std::fs::remove_file(path);
    }
}

/// Collects the sessions that ended without closing properly (their files are deleted)
/// and returns a recovery for each that left an autosave newer than its project file.
/// `autosave_for` gives the autosave path of a project (or of an unsaved song).
pub fn find_recoveries(
    data_dir: &Path,
    autosave_for: &dyn Fn(Option<&Path>) -> Option<PathBuf>,
) -> Vec<Recovery> {
    let Ok(entries) = std::fs::read_dir(sessions_dir(data_dir)) else {
        return Vec::new();
    };
    let mut out: Vec<Recovery> = Vec::new();
    for path in entries.flatten().map(|e| e.path()) {
        let Some(info) = take_stale(&path) else {
            continue;
        };
        let Some(autosave) = autosave_for(info.project.as_deref()) else {
            continue;
        };
        let Ok(modified) = std::fs::metadata(&autosave).and_then(|m| m.modified()) else {
            continue;
        };
        let project_time = info
            .project
            .as_ref()
            .and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
        // Only worth offering if the autosave is newer than the saved project.
        if project_time.is_some_and(|t| t >= modified) || out.iter().any(|r| r.autosave == autosave)
        {
            continue;
        }
        out.push(Recovery {
            autosave,
            project: info.project,
        });
    }
    out
}

/// Reads and deletes a session file whose owner is gone (`None` if it is still running).
fn take_stale(path: &Path) -> Option<SessionInfo> {
    if path.extension().and_then(|e| e.to_str()) != Some("lock") {
        return None;
    }
    let mut file = OpenOptions::new().read(true).write(true).open(path).ok()?;
    file.try_lock().ok()?;
    let mut text = String::new();
    let _ = file.read_to_string(&mut text);
    drop(file);
    let _ = std::fs::remove_file(path);
    Some(serde_json::from_str(&text).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crashed_sessions_are_recovered_running_ones_are_not() {
        let dir = std::env::temp_dir().join(format!("musiton-session-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let auto = dir.join("auto.musiton");
        let finder = |_: Option<&Path>| Some(auto.clone());
        std::fs::write(&auto, "(version: 1)").unwrap();
        // Nothing ran before.
        assert!(find_recoveries(&dir, &finder).is_empty());

        // A running session (another window) is not offered.
        let mut running = SessionLock::acquire(&dir).unwrap();
        running.set_project(None);
        assert!(find_recoveries(&dir, &finder).is_empty());
        // A clean exit leaves nothing behind.
        running.release();
        assert!(find_recoveries(&dir, &finder).is_empty());

        // A crash: the process ends without `release` (the OS drops the lock).
        let mut crashed = SessionLock::acquire(&dir).unwrap();
        crashed.set_project(None);
        drop(crashed);
        let r = find_recoveries(&dir, &finder);
        assert_eq!(
            r,
            vec![Recovery {
                autosave: auto.clone(),
                project: None
            }]
        );
        // Offered once only.
        assert!(find_recoveries(&dir, &finder).is_empty());

        // A project saved after the autosave needs no recovery.
        let project = dir.join("song.musiton");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&project, "(version: 1)").unwrap();
        let mut crashed = SessionLock::acquire(&dir).unwrap();
        crashed.set_project(Some(&project));
        drop(crashed);
        assert!(find_recoveries(&dir, &finder).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
