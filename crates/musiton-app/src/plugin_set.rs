//! The plugin instances of the open song (UI thread).
//!
//! Instances are created when a plugin device appears in the song and kept until another
//! song is opened, so undoing and redoing the removal of a plugin keeps its settings.
//! A plugin's settings are its own state, saved as `plugins/<device id>.state` next to the
//! song file (and next to the autosave copy).
//!
//! A device whose plugin is not installed (or does not load) stays in the song as a
//! placeholder: its saved state is kept in memory and written again on every save, and
//! after the next plugin scan it is loaded again, also from a new install location.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use musiton_core::ids::DeviceId;
use musiton_plugin_host::{Plugin, PluginFormat, PluginInfo};
use musiton_project::{DeviceKind, Project};
use musiton_ui::device_panel::PluginView;

use crate::engine_graph::{PluginNodes, PluginParams};

/// Where the state of plugin device `id` is saved inside `dir` (a `plugins/` folder).
pub fn state_file(dir: &Path, id: DeviceId) -> PathBuf {
    dir.join(format!("{}.state", id.0))
}

/// The `plugins/` folder next to the song file `song`.
pub fn states_dir(song: &Path) -> Option<PathBuf> {
    Some(song.parent()?.join("plugins"))
}

/// The plugin devices of `project`: (device, kind).
fn plugin_devices(project: &Project) -> impl Iterator<Item = (DeviceId, &DeviceKind)> {
    project
        .tracks
        .iter()
        .flat_map(|t| &t.devices)
        .filter(|d| d.is_plugin())
        .map(|d| (d.id, &d.kind))
}

/// Loads the plugin of `kind` and gives it the saved state from `state`, if any. When
/// the plugin file is gone, an installed plugin with the same id from `installed` is used.
fn instantiate(
    kind: &DeviceKind,
    state: Option<&[u8]>,
    rate: f64,
    block: usize,
    installed: &[PluginInfo],
) -> Result<Plugin, String> {
    let DeviceKind::Plugin {
        format,
        path,
        plugin_id,
        sandbox,
        name,
        ..
    } = kind
    else {
        return Err("not a plugin".into());
    };
    let format = match format.as_str() {
        "VST3" => PluginFormat::Vst3,
        "CLAP" => PluginFormat::Clap,
        other => return Err(format!("unknown plugin format {other}")),
    };
    let moved = installed
        .iter()
        .find(|p| p.format == format && p.id == *plugin_id)
        .map(|p| p.path.as_path());
    let file = match Path::new(path) {
        f if f.exists() => f,
        f => moved.filter(|m| m.exists()).ok_or_else(|| {
            format!(
                "{name} is not installed on this computer (looked for {}). Its settings \
                 are kept; it plays again once it is installed and found by a plugin scan.",
                f.display()
            )
        })?,
    };
    let sandboxed = |why: Option<String>| {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        Plugin::load_sandboxed(&exe, format, file, plugin_id, rate, block).map_err(|e| match &why {
            Some(first) => format!("{first}; in the sandbox: {e}"),
            None => e.to_string(),
        })
    };
    let plugin = if *sandbox {
        sandboxed(None)?
    } else {
        match Plugin::load(format, file, plugin_id, rate, block) {
            Ok(p) => p,
            // A plugin that fails inside Musiton gets a second chance in the sandbox.
            Err(e) => {
                tracing::warn!("{path} failed to load ({e}); trying the sandbox");
                sandboxed(Some(e.to_string()))?
            }
        }
    };
    if let Some(state) = state {
        if let Err(e) = plugin.set_state(state) {
            tracing::warn!("restoring the state of {path} failed: {e}");
        }
    }
    Ok(plugin)
}

/// Plugin instances of the song.
pub struct PluginSet {
    instances: HashMap<DeviceId, Plugin>,
    /// Devices whose plugin could not be loaded, with the reason.
    failed: HashMap<DeviceId, String>,
    /// Instances that are no longer used but may still be in a graph; dropped once free.
    retired: Vec<Plugin>,
    /// Where saved states are read from.
    state_dir: Option<PathBuf>,
    rate: f64,
    block: usize,
    /// Latency of each instance when the graph was last built (for delay compensation).
    latencies: HashMap<DeviceId, u32>,
    /// The device settings each instance was made for (a change reloads it).
    kinds: HashMap<DeviceId, DeviceKind>,
    /// Saved states of devices whose plugin did not load, kept so saving does not lose them.
    placeholders: HashMap<DeviceId, Vec<u8>>,
    /// The installed plugins from the last scan (to find plugins that moved).
    installed: Vec<PluginInfo>,
}

impl Default for PluginSet {
    fn default() -> Self {
        Self {
            instances: HashMap::new(),
            failed: HashMap::new(),
            retired: Vec::new(),
            state_dir: None,
            rate: 48_000.0,
            block: 1024,
            latencies: HashMap::new(),
            kinds: HashMap::new(),
            placeholders: HashMap::new(),
            installed: Vec::new(),
        }
    }
}

impl PluginSet {
    /// Forgets all instances (another song was opened); states load from `state_dir`.
    pub fn reset(&mut self, state_dir: Option<PathBuf>) {
        for p in self.instances.values_mut() {
            p.close_editor();
        }
        self.retired.extend(self.instances.drain().map(|(_, p)| p));
        self.failed.clear();
        self.kinds.clear();
        self.placeholders.clear();
        self.state_dir = state_dir;
    }

    /// Takes the plugin list of a finished scan and tries every device that did not load
    /// again (a plugin may have been installed since).
    pub fn set_installed(&mut self, installed: &[PluginInfo]) {
        self.installed = installed.to_vec();
        self.failed.clear();
    }

    /// Sets the engine's sample rate and block size; reloads every instance (keeping its
    /// state) when they change. Returns whether anything was reloaded.
    pub fn configure(&mut self, project: &Project, rate: f64, block: usize) -> bool {
        if (rate - self.rate).abs() < 0.5 && block == self.block {
            return false;
        }
        self.rate = rate;
        self.block = block;
        let kinds: HashMap<DeviceId, DeviceKind> = plugin_devices(project)
            .map(|(id, k)| (id, k.clone()))
            .collect();
        let old: Vec<(DeviceId, Plugin)> = self.instances.drain().collect();
        for (id, plugin) in old {
            let state = plugin.state().ok();
            if let Some(kind) = kinds.get(&id) {
                match instantiate(kind, state.as_deref(), rate, block, &self.installed) {
                    Ok(p) => {
                        self.instances.insert(id, p);
                    }
                    Err(e) => {
                        if let Some(state) = state {
                            self.placeholders.insert(id, state);
                        }
                        self.failed.insert(id, e);
                    }
                }
            }
            self.retired.push(plugin);
        }
        true
    }

    /// Loads the plugins of devices that appeared in `project`. Returns whether any
    /// instance was added.
    pub fn sync(&mut self, project: &Project) -> bool {
        let mut added = false;
        // Devices whose settings changed (e.g. the sandbox switch) are reloaded with their
        // current state.
        let mut carried: HashMap<DeviceId, Vec<u8>> = HashMap::new();
        for (id, kind) in plugin_devices(project) {
            if self.kinds.get(&id).is_some_and(|k| k != kind) {
                self.failed.remove(&id);
                if let Some(old) = self.instances.remove(&id) {
                    if let Ok(state) = old.state() {
                        carried.insert(id, state);
                    }
                    self.retired.push(old);
                }
            }
        }
        let todo: Vec<(DeviceId, DeviceKind)> = plugin_devices(project)
            .filter(|(id, _)| !self.instances.contains_key(id) && !self.failed.contains_key(id))
            .map(|(id, k)| (id, k.clone()))
            .collect();
        for (id, kind) in todo {
            self.kinds.insert(id, kind.clone());
            let state = carried
                .remove(&id)
                .or_else(|| self.placeholders.remove(&id))
                .or_else(|| {
                    self.state_dir
                        .as_deref()
                        .and_then(|d| std::fs::read(state_file(d, id)).ok())
                });
            let loaded = instantiate(
                &kind,
                state.as_deref(),
                self.rate,
                self.block,
                &self.installed,
            );
            match loaded {
                Ok(p) => {
                    self.instances.insert(id, p);
                    added = true;
                }
                Err(e) => {
                    tracing::warn!("loading plugin device {} failed: {e}", id.0);
                    if let Some(state) = state {
                        self.placeholders.insert(id, state);
                    }
                    self.failed.insert(id, e);
                }
            }
        }
        added
    }

    /// Whether a plugin's latency changed since the last call (the graph's delay
    /// compensation then has to be rebuilt).
    pub fn latency_changed(&mut self) -> bool {
        let now: HashMap<DeviceId, u32> = self
            .instances
            .iter()
            .map(|(id, p)| (*id, p.latency()))
            .collect();
        let changed = now != self.latencies;
        self.latencies = now;
        changed
    }

    /// Node makers and parameter lists for building a graph.
    pub fn nodes(&self) -> PluginNodes {
        PluginNodes {
            sources: self
                .instances
                .iter()
                .map(|(id, p)| (*id, p.source()))
                .collect(),
            params: self.params(),
        }
    }

    /// Parameter names of every instance (for automation lanes).
    pub fn params(&self) -> PluginParams {
        self.instances
            .iter()
            .map(|(id, p)| {
                (
                    *id,
                    p.params()
                        .iter()
                        .filter(|x| x.automatable)
                        .map(|x| (x.id, x.name.clone()))
                        .collect(),
                )
            })
            .collect()
    }

    /// What the device panel shows for device `id`.
    pub fn view(&self, id: DeviceId) -> Option<PluginView> {
        if let Some(e) = self.failed.get(&id) {
            return Some(PluginView {
                error: Some(e.clone()),
                ..PluginView::default()
            });
        }
        let p = self.instances.get(&id)?;
        if let Some(e) = p.failure() {
            return Some(PluginView {
                error: Some(e),
                ..PluginView::default()
            });
        }
        Some(PluginView {
            params: p
                .params()
                .iter()
                .map(|x| {
                    let v = p.param_value(x.id);
                    (x.id, x.name.clone(), v, p.param_text(x.id, v))
                })
                .collect(),
            error: None,
            has_editor: p.has_editor(),
        })
    }

    /// Shows a new parameter value in the plugin (the engine gets it separately).
    pub fn set_param(&self, id: DeviceId, param: u32, value: f64) {
        if let Some(p) = self.instances.get(&id) {
            p.set_param(param, value);
        }
    }

    /// Loads device `id`'s plugin again (after it failed or its sandbox ended); it starts
    /// from its last saved state.
    pub fn restart(&mut self, id: DeviceId) {
        self.failed.remove(&id);
        if let Some(old) = self.instances.remove(&id) {
            self.retired.push(old);
        }
        self.kinds.remove(&id);
    }

    /// Opens the editor window of device `id` titled `title`.
    pub fn open_editor(&mut self, id: DeviceId, title: &str) -> Result<(), String> {
        let p = self
            .instances
            .get_mut(&id)
            .ok_or_else(|| "the plugin is not loaded".to_owned())?;
        p.open_editor(title).map_err(|e| e.to_string())
    }

    /// Passes edits from the plugins' own editors on and frees retired instances.
    /// Returns the parameter moves made in the plugins' editors: (device, parameter,
    /// value 0..1).
    pub fn poll(&mut self) -> Vec<(DeviceId, u32, f64)> {
        let mut moves = Vec::new();
        for (id, p) in &mut self.instances {
            p.poll_editor();
            for e in p.take_edits() {
                if let musiton_plugin_host::Edit::Perform(param, value) = e {
                    moves.push((*id, param, value));
                }
            }
        }
        self.retired.retain(Plugin::in_use);
        moves
    }

    /// Writes the state of every plugin device of `project` into `dir`.
    pub fn save_states(&self, project: &Project, dir: &Path) -> Result<(), String> {
        let ids: Vec<DeviceId> = plugin_devices(project).map(|(id, _)| id).collect();
        if ids.is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        for id in ids {
            if let Some(p) = self.instances.get(&id) {
                let state = p.state().map_err(|e| e.to_string())?;
                std::fs::write(state_file(dir, id), state).map_err(|e| e.to_string())?;
            } else if let Some(state) = self.placeholders.get(&id) {
                std::fs::write(state_file(dir, id), state).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    /// Fresh instances with the current states at `rate`, for an export (it must not
    /// share instances with the live engine). Keep the returned plugins alive until the
    /// export has finished.
    pub fn for_export(
        &self,
        project: &Project,
        rate: f64,
        block: usize,
    ) -> (Vec<Plugin>, PluginNodes) {
        let mut keep = Vec::new();
        let mut nodes = PluginNodes::default();
        for (id, kind) in plugin_devices(project) {
            let Some(live) = self.instances.get(&id) else {
                continue;
            };
            let state = live.state().ok();
            match instantiate(kind, state.as_deref(), rate, block, &self.installed) {
                Ok(p) => {
                    nodes.sources.insert(id, p.source());
                    keep.push(p);
                }
                Err(e) => tracing::warn!("plugin for export failed: {e}"),
            }
        }
        (keep, nodes)
    }

    /// Hands plugins (e.g. of a finished export) over to be freed once unused.
    pub fn retire(&mut self, plugins: Vec<Plugin>) {
        self.retired.extend(plugins);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use musiton_project::{DeviceSlot, Track, TrackKind};

    fn kars_project() -> Option<Project> {
        let path = PathBuf::from("/usr/lib/clap/Kars.clap");
        let info = musiton_plugin_host::scan_file(&path)
            .ok()?
            .into_iter()
            .next()?;
        let mut p = Project::default();
        p.tracks.push(Track {
            kind: TrackKind::Instrument,
            devices: vec![DeviceSlot {
                id: DeviceId(5),
                bypass: false,
                sidechain: None,
                kind: DeviceKind::Plugin {
                    format: "CLAP".into(),
                    path: path.display().to_string(),
                    plugin_id: info.id,
                    name: info.name,
                    instrument: true,
                    sandbox: false,
                },
            }],
            ..Track::default()
        });
        Some(p)
    }

    #[test]
    fn plugin_states_are_saved_and_reloaded() {
        let Some(project) = kars_project() else {
            eprintln!("test plugin not installed; skipping");
            return;
        };
        let mut set = PluginSet::default();
        assert!(set.sync(&project));
        assert!(!set.sync(&project));
        let view = set.view(DeviceId(5)).unwrap();
        assert!(view.error.is_none() && !view.params.is_empty());
        let dir = std::env::temp_dir().join(format!("musiton-pstate-{}", std::process::id()));
        set.save_states(&project, &dir).unwrap();
        assert!(state_file(&dir, DeviceId(5)).is_file());

        // Another song folder with the same device reads the saved state.
        let mut again = PluginSet::default();
        again.reset(Some(dir.clone()));
        assert!(again.sync(&project));
        assert!(again.nodes().sources.contains_key(&DeviceId(5)));
        // A new rate reloads the instance.
        assert!(again.configure(&project, 44_100.0, 512));
        assert!(again.view(DeviceId(5)).is_some_and(|v| v.error.is_none()));
        let _ = std::fs::remove_dir_all(&dir);

        // A missing plugin shows why.
        let mut broken = project.clone();
        if let DeviceKind::Plugin { path, .. } = &mut broken.tracks[0].devices[0].kind {
            *path = "/nonexistent/X.clap".into();
        }
        let mut set = PluginSet::default();
        set.sync(&broken);
        assert!(set.view(DeviceId(5)).is_some_and(|v| v.error.is_some()));
    }

    #[test]
    fn a_missing_plugin_keeps_its_settings_and_loads_once_installed() {
        let Some(project) = kars_project() else {
            eprintln!("test plugin not installed; skipping");
            return;
        };
        let dir = std::env::temp_dir().join(format!("musiton-missing-{}", std::process::id()));
        let saved = dir.join("old");
        std::fs::create_dir_all(&saved).unwrap();
        std::fs::write(state_file(&saved, DeviceId(5)), b"saved settings").unwrap();
        // The song points at a place where the plugin is not installed.
        let mut moved = project.clone();
        let DeviceKind::Plugin { path, .. } = &mut moved.tracks[0].devices[0].kind else {
            unreachable!()
        };
        let real = std::mem::replace(path, "/nonexistent/Kars.clap".into());
        let mut set = PluginSet::default();
        set.reset(Some(saved));
        assert!(!set.sync(&moved));
        let error = set.view(DeviceId(5)).and_then(|v| v.error).unwrap();
        assert!(error.contains("not installed"), "{error}");
        // Saving to another folder still writes the settings it had.
        let new = dir.join("new");
        set.save_states(&moved, &new).unwrap();
        assert_eq!(
            std::fs::read(state_file(&new, DeviceId(5))).unwrap(),
            b"saved settings"
        );
        // A scan finds the plugin somewhere else: it loads from there.
        let info = musiton_plugin_host::scan_file(Path::new(&real)).unwrap();
        set.set_installed(&info);
        assert!(set.sync(&moved));
        assert!(set.view(DeviceId(5)).is_some_and(|v| v.error.is_none()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_plugin_synth_track_renders_its_notes() {
        use musiton_core::ids::ClipId;
        use musiton_core::{Tick, PPQ};
        use musiton_project::{Clip, MidiClip, Note};
        use std::sync::atomic::AtomicBool;
        let Some(mut project) = kars_project() else {
            eprintln!("test plugin not installed; skipping");
            return;
        };
        project.tracks[0].clips.push(Clip::Midi(MidiClip {
            id: ClipId(9),
            start: Tick(0),
            length: Tick(4 * PPQ),
            notes: vec![Note {
                start: Tick(0),
                length: Tick(2 * PPQ),
                pitch: 60,
                velocity: 100,
                channel: 0,
            }],
            cc: Vec::new(),
        }));
        let mut set = PluginSet::default();
        set.sync(&project);
        let (_keep, nodes) = set.for_export(&project, 48_000.0, crate::export::BLOCK);
        let out = crate::export::render_project(
            &project,
            &crate::assets::AssetStore::default(),
            &nodes,
            0,
            48_000,
            &|_| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        let peak = out[0].iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.01, "peak {peak}");
        // Without the plugin loaded the track stays silent.
        let silent = crate::export::render_project(
            &project,
            &crate::assets::AssetStore::default(),
            &PluginNodes::default(),
            0,
            24_000,
            &|_| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(silent[0].iter().all(|s| *s == 0.0));
    }
}
