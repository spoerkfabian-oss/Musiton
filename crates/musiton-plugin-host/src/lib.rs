//! Plugin hosting for Musiton: VST3 and CLAP.
//!
//! This is the only crate that may use `unsafe` (the plugin APIs are C/COM interfaces).
//! Every `unsafe` block has a `// SAFETY:` comment.
//!
//! Scanning a plugin file loads foreign code, which may crash. The app therefore never
//! scans in its own process: it runs `musiton-scan` (see `tools/scanner`), which calls
//! [`scan_file`] and prints the result as JSON.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

mod clap;
mod plugin;
mod vst3_host;
mod vst3_module;
mod vst3_plugin;

pub use plugin::{Edit, NodeSource, Plugin, PluginNode, PluginParam};

/// Plugin formats Musiton hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PluginFormat {
    /// Steinberg VST 3.
    Vst3,
    /// CLever Audio Plugin.
    Clap,
}

impl PluginFormat {
    /// Format of a plugin file, judged by its extension.
    pub fn of(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "vst3" => Some(Self::Vst3),
            "clap" => Some(Self::Clap),
            _ => None,
        }
    }

    /// Display name ("VST3", "CLAP").
    pub fn name(self) -> &'static str {
        match self {
            Self::Vst3 => "VST3",
            Self::Clap => "CLAP",
        }
    }
}

/// One plugin found in a plugin file (a file can hold several).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PluginInfo {
    /// File format.
    pub format: PluginFormat,
    /// The plugin file or bundle.
    pub path: PathBuf,
    /// Unique id: the VST3 class id as 32 hex digits, or the CLAP plugin id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Maker.
    pub vendor: String,
    /// Categories as the plugin reports them (e.g. "Instrument|Synth", "audio-effect reverb").
    pub category: String,
    /// Whether it is an instrument (takes notes, makes sound) rather than an effect.
    pub instrument: bool,
    /// Version string.
    pub version: String,
}

/// Errors from loading or scanning plugins.
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    /// Not a `.vst3` or `.clap` file.
    #[error("not a plugin file: {0}")]
    NotAPlugin(PathBuf),
    /// The binary inside the bundle is missing.
    #[error("no plugin binary for this system in {0}")]
    NoBinary(PathBuf),
    /// The shared library could not be loaded or lacks the entry point.
    #[error("could not load {path}: {reason}")]
    Load {
        /// The file.
        path: PathBuf,
        /// Why.
        reason: String,
    },
    /// The plugin refused or failed a call.
    #[error("the plugin failed: {0}")]
    Failed(String),
}

/// Plugins are loaded, scanned and shut down one at a time per process: many plugins
/// keep global state that is not safe to set up or tear down from two threads at once.
pub(crate) fn lifecycle_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Lists the plugins in one plugin file. Loads foreign code: call it only in a process
/// that may crash (the scanner).
pub fn scan_file(path: &Path) -> Result<Vec<PluginInfo>, PluginError> {
    let _one_at_a_time = lifecycle_lock();
    match PluginFormat::of(path) {
        Some(PluginFormat::Vst3) => vst3_module::scan(path),
        Some(PluginFormat::Clap) => clap::scan(path),
        None => Err(PluginError::NotAPlugin(path.to_owned())),
    }
}

/// The standard plugin folders of this system.
pub fn default_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(windows) {
        for var in ["COMMONPROGRAMFILES", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(var).map(PathBuf::from) {
                let base = if var == "LOCALAPPDATA" {
                    base.join("Programs").join("Common")
                } else {
                    base
                };
                dirs.push(base.join("VST3"));
                dirs.push(base.join("CLAP"));
            }
        }
    } else {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            dirs.push(home.join(".vst3"));
            dirs.push(home.join(".clap"));
        }
        for d in [
            "/usr/lib/vst3",
            "/usr/local/lib/vst3",
            "/usr/lib/clap",
            "/usr/local/lib/clap",
        ] {
            dirs.push(PathBuf::from(d));
        }
    }
    dirs
}

/// Plugin files (`.vst3` bundles or files, `.clap` files) under `dirs`, sorted.
pub fn find_plugin_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let path = e.path();
            if PluginFormat::of(&path).is_some() {
                // A bundle is a folder; never look inside it for more plugins.
                out.push(path);
            } else if depth < 4 && path.is_dir() {
                walk(&path, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    for d in dirs {
        walk(d, 0, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

/// Text of a fixed-size, NUL-terminated C string field.
fn c_text(chars: &[std::ffi::c_char]) -> String {
    let bytes: Vec<u8> = chars
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test plugins of the Linux CI image (`dpf-plugins-vst3`, `dpf-plugins-clap`).
    fn kars(ext: &str) -> Option<PathBuf> {
        let p = PathBuf::from(format!("/usr/lib/{ext}/Kars.{ext}"));
        p.exists().then_some(p)
    }

    #[test]
    fn plugin_files_are_found_by_extension() {
        assert_eq!(
            PluginFormat::of(Path::new("a/B.VST3")),
            Some(PluginFormat::Vst3)
        );
        assert_eq!(
            PluginFormat::of(Path::new("x.clap")),
            Some(PluginFormat::Clap)
        );
        assert_eq!(PluginFormat::of(Path::new("x.dll")), None);
        let dirs = default_dirs();
        assert!(!dirs.is_empty());
        if kars("vst3").is_some() {
            let found = find_plugin_files(&dirs);
            assert!(found.iter().any(|p| p.ends_with("Kars.vst3")), "{found:?}");
            assert!(found.iter().any(|p| p.ends_with("Kars.clap")));
        }
    }

    #[test]
    fn vst3_and_clap_plugins_are_scanned() {
        let (Some(vst3), Some(clap)) = (kars("vst3"), kars("clap")) else {
            eprintln!("test plugins not installed; skipping");
            return;
        };
        for path in [vst3, clap] {
            let found = scan_file(&path).unwrap();
            assert_eq!(found.len(), 1, "{found:?}");
            let p = &found[0];
            assert_eq!(p.name, "Kars");
            assert!(p.instrument, "{p:?}");
            assert!(!p.vendor.is_empty() && !p.id.is_empty(), "{p:?}");
        }
        let reverb = PathBuf::from("/usr/lib/vst3/MVerb.vst3");
        let found = scan_file(&reverb).unwrap();
        assert!(!found[0].instrument, "{found:?}");
        assert!(matches!(
            scan_file(Path::new("/nonexistent/X.vst3")),
            Err(PluginError::NoBinary(_) | PluginError::Load { .. })
        ));
    }
}
