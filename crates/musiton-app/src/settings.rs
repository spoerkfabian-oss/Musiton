//! User settings persisted as `settings.json` in the OS config folder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// All persisted user settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Audio device settings.
    pub audio: AudioSettings,
}

/// Audio device settings; `None` means "device default".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    /// Output device id.
    pub output_device: Option<String>,
    /// Sample rate in Hz.
    pub sample_rate: Option<u32>,
    /// Buffer size in frames.
    pub buffer_size: Option<u32>,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            output_device: None,
            sample_rate: None,
            buffer_size: Some(256),
        }
    }
}

impl Settings {
    /// Default location: `%APPDATA%\Musiton\config\settings.json` on Windows.
    pub fn default_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "Musiton")
            .map(|d| d.config_dir().join("settings.json"))
    }

    /// Loads settings; a missing or broken file gives defaults (logged, never fatal).
    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!("ignoring broken settings file {}: {e}", path.display());
                Self::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                tracing::warn!("cannot read settings {}: {e}", path.display());
                Self::default()
            }
        }
    }

    /// Saves settings atomically (write temp file, then rename).
    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("musiton-test-{}-{name}", std::process::id()))
            .join("settings.json")
    }

    #[test]
    fn save_and_load_round_trip() {
        let path = temp_path("roundtrip");
        let s = Settings {
            audio: AudioSettings {
                output_device: Some("wasapi:{abc}".into()),
                sample_rate: Some(48_000),
                buffer_size: Some(128),
            },
        };
        s.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path), s);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn missing_or_broken_file_gives_defaults() {
        let path = temp_path("broken");
        assert_eq!(Settings::load_from(&path), Settings::default());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(Settings::load_from(&path), Settings::default());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn unknown_and_missing_fields_are_tolerated() {
        let s: Settings = serde_json::from_str(r#"{"audio":{"sample_rate":44100},"x":1}"#).unwrap();
        assert_eq!(s.audio.sample_rate, Some(44_100));
        assert_eq!(s.audio.buffer_size, Some(256));
    }
}
