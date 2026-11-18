//! CLAP plugins (through `clack-host`).

use std::path::Path;

use clack_host::entry::PluginEntry;

use crate::{PluginError, PluginFormat, PluginInfo};

/// Loads the CLAP file at `path`.
pub(crate) fn load(path: &Path) -> Result<PluginEntry, PluginError> {
    if !path.is_file() {
        return Err(PluginError::NoBinary(path.to_owned()));
    }
    // SAFETY: loading runs the plugin's initializers; the user installed the plugin, and
    // scanning happens in a separate process.
    unsafe { PluginEntry::load(path) }.map_err(|e| PluginError::Load {
        path: path.to_owned(),
        reason: e.to_string(),
    })
}

/// Lists the plugins of a `.clap` file.
pub(crate) fn scan(path: &Path) -> Result<Vec<PluginInfo>, PluginError> {
    let entry = load(path)?;
    let factory = entry
        .get_plugin_factory()
        .ok_or_else(|| PluginError::Failed("no plugin factory".into()))?;
    let text = |s: Option<&std::ffi::CStr>| s.map(|s| s.to_string_lossy().into_owned());
    Ok(factory
        .plugin_descriptors()
        .filter_map(|d| {
            let features: Vec<String> = d
                .features()
                .map(|f| f.to_string_lossy().into_owned())
                .collect();
            Some(PluginInfo {
                format: PluginFormat::Clap,
                path: path.to_owned(),
                id: text(d.id())?,
                name: text(d.name()).unwrap_or_default(),
                vendor: text(d.vendor()).unwrap_or_default(),
                instrument: features.iter().any(|f| f == "instrument"),
                category: features.join(" "),
                version: text(d.version()).unwrap_or_default(),
            })
        })
        .collect())
}
