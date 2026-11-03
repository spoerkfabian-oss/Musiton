//! User presets: `<data dir>/presets/<device type>/<name>.ron`.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::settings::Settings;

/// Parameter values of a preset.
pub type PresetValues = Vec<(u64, f32)>;
/// Presets by device type: (name, values).
pub type PresetMap = HashMap<String, Vec<(String, PresetValues)>>;

fn dir() -> Option<PathBuf> {
    Settings::data_dir().map(|d| d.join("presets"))
}

/// Saves a preset (overwrites one with the same name).
pub fn save(type_id: &str, name: &str, params: &[(u64, f32)]) -> anyhow::Result<()> {
    let dir = dir()
        .ok_or_else(|| anyhow::anyhow!("no data folder"))?
        .join(type_id);
    std::fs::create_dir_all(&dir)?;
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    std::fs::write(dir.join(format!("{safe}.ron")), ron::to_string(params)?)?;
    Ok(())
}

/// Loads all user presets, sorted by name. Broken files are skipped.
pub fn load_all() -> PresetMap {
    let mut out = PresetMap::new();
    let Some(root) = dir() else {
        return out;
    };
    let Ok(types) = std::fs::read_dir(&root) else {
        return out;
    };
    for ty in types.filter_map(Result::ok) {
        let type_id = ty.file_name().to_string_lossy().into_owned();
        let Ok(files) = std::fs::read_dir(ty.path()) else {
            continue;
        };
        let mut list: Vec<(String, PresetValues)> = files
            .filter_map(Result::ok)
            .filter_map(|f| {
                let path = f.path();
                let name = path.file_stem()?.to_string_lossy().into_owned();
                let values = ron::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
                Some((name, values))
            })
            .collect();
        list.sort_by(|a, b| a.0.cmp(&b.0));
        out.insert(type_id, list);
    }
    out
}
