//! User settings persisted as `settings.json` in the OS config folder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// All persisted user settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Audio device settings.
    pub audio: AudioSettings,
    /// Recently opened projects, newest first.
    pub recent_projects: Vec<PathBuf>,
    /// Folder shown in the browser panel.
    pub browser_dir: Option<PathBuf>,
}

/// Most entries kept in [`Settings::recent_projects`].
const MAX_RECENT: usize = 10;

/// Audio device settings; `None` means "device default".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    /// Output device id.
    pub output_device: Option<String>,
    /// Sample rate in Hz.
    pub sample_rate: Option<u32>,
    /// Buffer size in frames.
    pub buffer_size: Option<u32>,
    /// Use an audio input (for recording and monitoring).
    pub input_enabled: bool,
    /// Input device id (`None` = system default).
    pub input_device: Option<String>,
    /// Measured round-trip latency in frames (from "Measure").
    pub measured_latency: Option<u32>,
    /// Extra correction for recordings in milliseconds (positive = move earlier).
    pub record_offset_ms: f32,
}

impl AudioSettings {
    /// How many frames recorded audio is moved earlier: the measured round trip if there
    /// is one, else the buffer sizes (`estimate`), plus the manual correction.
    pub fn record_latency(&self, estimate: u32, sample_rate: u32) -> i64 {
        let base = i64::from(self.measured_latency.unwrap_or(estimate));
        base + (f64::from(self.record_offset_ms) * f64::from(sample_rate) / 1000.0).round() as i64
    }
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            output_device: None,
            sample_rate: None,
            buffer_size: Some(256),
            input_enabled: true,
            input_device: None,
            measured_latency: None,
            record_offset_ms: 0.0,
        }
    }
}

impl Settings {
    /// Puts `path` at the top of the recent-projects list.
    pub fn add_recent(&mut self, path: &Path) {
        self.recent_projects.retain(|p| p != path);
        self.recent_projects.insert(0, path.to_path_buf());
        self.recent_projects.truncate(MAX_RECENT);
    }

    /// Folder for files that belong to no project (e.g. autosave of an unsaved song).
    pub fn data_dir() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "Musiton").map(|d| d.data_dir().to_path_buf())
    }

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
                measured_latency: Some(512),
                record_offset_ms: -1.5,
                ..AudioSettings::default()
            },
            recent_projects: vec![PathBuf::from("a/b.musiton")],
            browser_dir: Some(PathBuf::from("samples")),
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
    fn recent_list_is_unique_newest_first_and_capped() {
        let mut s = Settings::default();
        for i in 0..12 {
            s.add_recent(Path::new(&format!("{i}.musiton")));
        }
        s.add_recent(Path::new("5.musiton"));
        assert_eq!(s.recent_projects.len(), MAX_RECENT);
        assert_eq!(s.recent_projects[0], PathBuf::from("5.musiton"));
        assert_eq!(
            s.recent_projects
                .iter()
                .filter(|p| p.ends_with("5.musiton"))
                .count(),
            1
        );
    }

    #[test]
    fn record_latency_prefers_measurement_and_adds_offset() {
        let mut a = AudioSettings::default();
        assert_eq!(a.record_latency(512, 48_000), 512);
        a.measured_latency = Some(700);
        a.record_offset_ms = 1.0;
        assert_eq!(a.record_latency(512, 48_000), 748);
    }

    #[test]
    fn unknown_and_missing_fields_are_tolerated() {
        let s: Settings = serde_json::from_str(r#"{"audio":{"sample_rate":44100},"x":1}"#).unwrap();
        assert_eq!(s.audio.sample_rate, Some(44_100));
        assert_eq!(s.audio.buffer_size, Some(256));
    }
}
