//! The plugin list: plugin files are scanned in a separate process (`musiton --scan
//! <file>`), so a plugin that crashes or hangs while loading cannot take Musiton down.
//! Results are cached in `plugins.json` in the data folder; failed files are remembered
//! and skipped until the user asks for a full rescan.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use eframe::egui;
use musiton_plugin_host::{find_plugin_files, scan_file, PluginInfo};
use serde::{Deserialize, Serialize};

/// Command-line switch that turns the executable into the scanner.
pub const SCAN_ARG: &str = "--scan";
/// A plugin taking longer than this to load is treated as hung.
const SCAN_TIMEOUT: Duration = Duration::from_secs(20);

/// Scanner mode: prints the plugins of `file` as JSON and exits (code 2 on failure).
pub fn scan_mode(file: &Path) -> ! {
    match scan_file(file) {
        Ok(list) => {
            println!("{}", serde_json::to_string(&list).unwrap_or_default());
            std::process::exit(0)
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2)
        }
    }
}

/// Runs `cmd` (a scanner or checker process) and parses its JSON output; kills it after
/// `timeout`.
pub fn run_scanner<T: serde::de::DeserializeOwned>(
    mut cmd: Command,
    timeout: Duration,
) -> Result<T, String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start the scanner: {e}"))?;
    // Read the pipes on threads so a chatty plugin cannot block on a full pipe.
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_string(&mut s);
            }
            s
        })
    };
    let out = read(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = read(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("did not finish within {} s", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let out = out.join().unwrap_or_default();
    let err = err.join().unwrap_or_default();
    if status.success() {
        serde_json::from_str(out.trim()).map_err(|e| format!("unreadable result: {e}"))
    } else if status.code() == Some(2) {
        Err(err.trim().to_owned())
    } else {
        Err(format!("crashed ({status})"))
    }
}

/// Seconds since 1970 of the last change of `path` (0 if unknown).
fn stamp(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// Known plugins and the files they came from.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginCache {
    /// Plugins found, sorted by vendor and name.
    pub plugins: Vec<PluginInfo>,
    /// Files that failed (crash, hang, error), with the reason. Skipped until a full rescan.
    pub failed: BTreeMap<PathBuf, String>,
    /// Change time of each scanned file when it was scanned.
    stamps: BTreeMap<PathBuf, u64>,
}

impl PluginCache {
    /// Reads the cache (empty if missing or unreadable).
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Writes the cache.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self).unwrap_or_default())
    }

    /// Of `files`, the ones to scan: new or changed since their scan, and not failed.
    pub fn stale(&self, files: &[PathBuf]) -> Vec<PathBuf> {
        files
            .iter()
            .filter(|f| !self.failed.contains_key(*f) && self.stamps.get(*f) != Some(&stamp(f)))
            .cloned()
            .collect()
    }

    /// Stores the scan result of `file`.
    pub fn record(&mut self, file: &Path, result: Result<Vec<PluginInfo>, String>) {
        self.plugins.retain(|p| p.path != file);
        self.failed.remove(file);
        match result {
            Ok(found) => self.plugins.extend(found),
            Err(reason) => {
                self.failed.insert(file.to_owned(), reason);
            }
        }
        self.stamps.insert(file.to_owned(), stamp(file));
        self.plugins.sort_by(|a, b| {
            (a.vendor.to_lowercase(), a.name.to_lowercase())
                .cmp(&(b.vendor.to_lowercase(), b.name.to_lowercase()))
        });
    }

    /// Forgets files that are no longer installed.
    pub fn prune(&mut self, files: &[PathBuf]) {
        self.plugins.retain(|p| files.contains(&p.path));
        self.failed.retain(|f, _| files.contains(f));
        self.stamps.retain(|f, _| files.contains(f));
    }

    /// Forgets the failures so the next scan tries those files again.
    pub fn clear_failed(&mut self) {
        for f in std::mem::take(&mut self.failed).into_keys() {
            self.stamps.remove(&f);
        }
    }
}

/// One finished file of a background scan.
type Scanned = (PathBuf, Result<Vec<PluginInfo>, String>);

/// The plugin list with its background scanner and window.
pub struct Plugins {
    /// The cached list.
    pub cache: PluginCache,
    cache_path: Option<PathBuf>,
    /// Extra folders to search (from the settings).
    pub extra_dirs: Vec<PathBuf>,
    scanning: Option<(mpsc::Receiver<Scanned>, usize, usize)>,
    /// Whether the plugin window is shown.
    pub open: bool,
    filter: String,
}

impl Plugins {
    /// Loads the cache from `cache_path`.
    pub fn new(cache_path: Option<PathBuf>) -> Self {
        Self {
            cache: cache_path
                .as_deref()
                .map(PluginCache::load)
                .unwrap_or_default(),
            cache_path,
            extra_dirs: Vec::new(),
            scanning: None,
            open: false,
            filter: String::new(),
        }
    }

    /// Starts scanning new and changed plugin files in the background (`all`: also the
    /// ones that failed before).
    pub fn scan(&mut self, all: bool) {
        if self.scanning.is_some() {
            return;
        }
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let mut dirs = musiton_plugin_host::default_dirs();
        dirs.extend(self.extra_dirs.iter().cloned());
        let files = find_plugin_files(&dirs);
        self.cache.prune(&files);
        if all {
            self.cache.clear_failed();
        }
        let todo = self.cache.stale(&files);
        if todo.is_empty() {
            self.save();
            return;
        }
        let total = todo.len();
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("plugin-scan".into())
            .spawn(move || {
                for file in todo {
                    let mut cmd = Command::new(&exe);
                    cmd.arg(SCAN_ARG).arg(&file);
                    let result = run_scanner(cmd, SCAN_TIMEOUT);
                    if let Err(e) = &result {
                        tracing::warn!("plugin scan of {} failed: {e}", file.display());
                    }
                    if tx.send((file, result)).is_err() {
                        return;
                    }
                }
            });
        if spawned.is_ok() {
            self.scanning = Some((rx, 0, total));
        }
    }

    /// Takes finished scan results; returns whether the scan is still running.
    pub fn update(&mut self) -> bool {
        let Some((rx, done, total)) = &mut self.scanning else {
            return false;
        };
        let mut finished = false;
        loop {
            match rx.try_recv() {
                Ok((file, result)) => {
                    self.cache.record(&file, result);
                    *done += 1;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }
        if finished || *done >= *total {
            self.scanning = None;
            self.save();
            return false;
        }
        true
    }

    fn save(&self) {
        if let Some(path) = &self.cache_path {
            if let Err(e) = self.cache.save(path) {
                tracing::warn!("saving the plugin list failed: {e}");
            }
        }
    }

    /// The plugin window: everything found, grouped by vendor, and what failed.
    pub fn window(&mut self, ctx: &egui::Context) {
        let mut open = self.open;
        egui::Window::new("Plugins")
            .open(&mut open)
            .default_size([520.0, 420.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some((_, done, total)) = &self.scanning {
                        ui.spinner();
                        ui.label(format!("Scanning {done} / {total}…"));
                    } else {
                        if ui
                            .button("Scan for new plugins")
                            .on_hover_text(
                                "Looks for plugins added or updated since the last scan.",
                            )
                            .clicked()
                        {
                            self.scan(false);
                        }
                        if ui
                            .button("Rescan all")
                            .on_hover_text("Also tries the plugins that failed before.")
                            .clicked()
                        {
                            self.scan(true);
                        }
                    }
                    ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter"));
                });
                ui.weak(format!(
                    "{} plugins. Folders: the standard VST3 and CLAP folders.",
                    self.cache.plugins.len()
                ));
                ui.separator();
                let filter = self.filter.to_lowercase();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut vendor = None;
                    for p in &self.cache.plugins {
                        let text = format!("{} {} {}", p.vendor, p.name, p.category).to_lowercase();
                        if !filter.is_empty() && !text.contains(&filter) {
                            continue;
                        }
                        if vendor != Some(&p.vendor) {
                            vendor = Some(&p.vendor);
                            ui.add_space(4.0);
                            ui.strong(if p.vendor.is_empty() {
                                "(unknown maker)"
                            } else {
                                &p.vendor
                            });
                        }
                        ui.horizontal(|ui| {
                            ui.label(&p.name);
                            ui.weak(format!(
                                "{} · {} · {}",
                                p.format.name(),
                                if p.instrument { "Instrument" } else { "Effect" },
                                p.category
                            ));
                        });
                    }
                    if !self.cache.failed.is_empty() {
                        ui.add_space(8.0);
                        ui.strong("Failed to load (skipped until “Rescan all”)");
                        for (file, reason) in &self.cache.failed {
                            ui.label(format!("{} — {reason}", file.display()));
                        }
                    }
                });
            });
        self.open = open;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use musiton_plugin_host::PluginFormat;

    fn info(path: &Path, name: &str, vendor: &str) -> PluginInfo {
        PluginInfo {
            format: PluginFormat::Clap,
            path: path.to_owned(),
            id: name.into(),
            name: name.into(),
            vendor: vendor.into(),
            category: String::new(),
            instrument: false,
            version: String::new(),
        }
    }

    #[test]
    fn cache_skips_scanned_and_failed_files_and_survives_a_round_trip() {
        let dir = std::env::temp_dir().join(format!("musiton-plugins-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("A.clap");
        let b = dir.join("B.clap");
        std::fs::write(&a, "x").unwrap();
        std::fs::write(&b, "x").unwrap();
        let files = vec![a.clone(), b.clone()];
        let mut cache = PluginCache::default();
        assert_eq!(cache.stale(&files), files);
        cache.record(
            &a,
            Ok(vec![info(&a, "Zed", "beta"), info(&a, "Alpha", "Beta")]),
        );
        cache.record(&b, Err("crashed".into()));
        assert!(cache.stale(&files).is_empty());
        assert_eq!(cache.plugins[0].name, "Alpha");

        let path = dir.join("plugins.json");
        cache.save(&path).unwrap();
        assert_eq!(PluginCache::load(&path), cache);

        // A full rescan retries the failed file; uninstalled files are forgotten.
        cache.clear_failed();
        assert_eq!(cache.stale(&files), vec![b.clone()]);
        cache.prune(std::slice::from_ref(&b));
        assert!(cache.plugins.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn hanging_and_crashing_scanners_are_reported() {
        let mut hang = Command::new("sleep");
        hang.arg("5");
        let err = run_scanner::<Vec<PluginInfo>>(hang, Duration::from_millis(200)).unwrap_err();
        assert!(err.contains("did not finish"), "{err}");
        let mut crash = Command::new("sh");
        crash.args(["-c", "kill -SEGV $$"]);
        let err = run_scanner::<Vec<PluginInfo>>(crash, Duration::from_secs(5)).unwrap_err();
        assert!(err.contains("crashed"), "{err}");
        let mut ok = Command::new("sh");
        ok.args(["-c", "echo '[]'"]);
        assert_eq!(
            run_scanner::<Vec<PluginInfo>>(ok, Duration::from_secs(5)).unwrap(),
            vec![]
        );
    }
}
