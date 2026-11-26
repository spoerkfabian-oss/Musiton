//! Genre sound packs: one-shot sounds (drums, basses, stabs …) rendered by Musiton's own
//! instruments into `<data dir>/Sounds/<genre>/`, shown by the browser's "Sounds" button.
//! They are made once (again when `PACK_VERSION` changes) and never overwrite other files.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use musiton_core::PPQ;
use musiton_devices::effects::{reverb, saturation};
use musiton_io::export::{write_export, ExportFormat, ExportOptions};

use crate::assets::AssetStore;
use crate::templates::{assemble, note, preset, Part};

/// Bump when the sounds change, so existing packs are made again.
const PACK_VERSION: &str = "5";
const SAMPLE_RATE: u32 = 48_000;
/// Render tempo: one beat = 0.5 s.
const BPM: f64 = 120.0;

/// One sound: file name, instrument, settings, effects, keys and how long they are held.
#[derive(Clone)]
struct Sound {
    name: &'static str,
    instrument: &'static str,
    params: Vec<(u64, f32)>,
    effects: Vec<(&'static str, Vec<(u64, f32)>)>,
    keys: Vec<u8>,
    /// Seconds the keys are held.
    hold: f64,
    /// Seconds rendered (the tail is trimmed where it falls silent).
    length: f64,
}

fn synth(name: &'static str, preset_name: &str, keys: &[u8], hold: f64, length: f64) -> Sound {
    Sound {
        name,
        instrument: musiton_devices::POLYSYNTH,
        params: preset(musiton_devices::POLYSYNTH, preset_name),
        effects: Vec::new(),
        keys: keys.to_vec(),
        hold,
        length,
    }
}

fn inst(
    name: &'static str,
    instrument: &'static str,
    keys: &[u8],
    hold: f64,
    length: f64,
) -> Sound {
    Sound {
        name,
        instrument,
        params: Vec::new(),
        effects: Vec::new(),
        keys: keys.to_vec(),
        hold,
        length,
    }
}

/// The drum synth's kick, snare, clap, hats (and cowbell) with `preset_name`.
fn kit(preset_name: &str, cowbell: bool) -> Vec<Sound> {
    let mut keys = vec![
        ("Kick", 36, 1.5),
        ("Snare", 38, 1.0),
        ("Clap", 39, 1.0),
        ("Hi-hat", 42, 0.5),
        ("Open hi-hat", 46, 1.5),
    ];
    if cowbell {
        keys.push(("Cowbell (808)", 56, 1.0));
    }
    keys.into_iter()
        .map(|(name, key, length)| Sound {
            name,
            instrument: "drums",
            params: preset("drums", preset_name),
            effects: Vec::new(),
            keys: vec![key],
            hold: 0.1,
            length,
        })
        .collect()
}

fn with_fx(mut s: Sound, fx: &[(&'static str, Vec<(u64, f32)>)]) -> Sound {
    s.effects.extend_from_slice(fx);
    s
}

fn drive(db: f32, kind: f32) -> (&'static str, Vec<(u64, f32)>) {
    (
        "saturation",
        vec![(saturation::id::DRIVE, db), (saturation::id::KIND, kind)],
    )
}

fn space(mix: f32) -> (&'static str, Vec<(u64, f32)>) {
    ("reverb", vec![(reverb::id::MIX, mix)])
}

/// Folder name of a genre ("/" is not allowed in file names).
fn folder(genre: &musiton_devices::genres::Genre) -> String {
    genre.name.replace(" / ", " & ")
}

/// The sounds of genre `id`.
fn sounds(id: &str) -> Vec<Sound> {
    match id {
        "raw" => [
            vec![
                synth(
                    "Raw stab C#maj7",
                    "Raw stab",
                    &[37, 44, 49, 53, 60],
                    0.3,
                    1.0,
                ),
                synth("Raw reese F", "Raw reese", &[29], 1.5, 2.0),
                synth("Dirty sub C#", "Dirty sub", &[25], 1.0, 1.5),
            ],
            kit("Punchy", false),
        ]
        .concat(),
        "phonk" => [
            vec![
                with_fx(
                    synth("Cowbell F#", "Phonk cowbell", &[66], 0.2, 1.0),
                    &[drive(10.0, 1.0), space(0.25)],
                ),
                with_fx(
                    synth("Cowbell C#", "Phonk cowbell", &[73], 0.2, 1.0),
                    &[drive(10.0, 1.0), space(0.25)],
                ),
                with_fx(
                    synth("808 F#", "Phonk 808", &[30], 1.5, 2.0),
                    &[drive(14.0, 2.0)],
                ),
            ],
            kit("Phonk", true),
        ]
        .concat(),
        "tekk" => [
            vec![
                with_fx(synth("Lead A", "Tekk lead", &[69], 0.4, 1.0), &[space(0.2)]),
                with_fx(synth("Bass A", "Bass", &[33], 0.2, 0.6), &[drive(8.0, 0.0)]),
            ],
            kit("Tekk", false)
                .into_iter()
                .map(|s| {
                    if s.name == "Kick" {
                        with_fx(s, &[drive(18.0, 1.0)])
                    } else {
                        s
                    }
                })
                .collect(),
        ]
        .concat(),
        "trap" => [
            vec![
                with_fx(
                    synth("808 C", "Phonk 808", &[24], 2.0, 2.5),
                    &[drive(8.0, 0.0)],
                ),
                with_fx(synth("Bell C", "Pluck", &[72], 0.3, 1.5), &[space(0.3)]),
            ],
            kit("808 style", true),
        ]
        .concat(),
        "house" => [
            vec![
                with_fx(
                    inst("Keys Am7", "epiano", &[57, 60, 64, 67], 0.3, 1.5),
                    &[space(0.2)],
                ),
                synth("Bass A", "Bass", &[33], 0.25, 0.6),
            ],
            kit("House", false),
        ]
        .concat(),
        "techno" => [
            vec![with_fx(inst("Acid A", "acid", &[33], 0.25, 0.6), &[])],
            kit("Punchy", false),
        ]
        .concat(),
        "hyperpop" => [
            vec![
                with_fx(
                    synth("Supersaw E", "Hyper supersaw", &[64, 68, 71], 0.5, 1.5),
                    &[space(0.25)],
                ),
                with_fx(
                    synth("808 E", "Phonk 808", &[28], 1.5, 2.0),
                    &[drive(16.0, 1.0)],
                ),
            ],
            kit("Punchy", false),
        ]
        .concat(),
        "hardstyle" => vec![
            Sound {
                name: "Kick",
                instrument: "drums",
                params: [
                    preset("drums", "Tekk"),
                    vec![
                        (musiton_devices::drums::id::KICK_DECAY, 0.6),
                        (musiton_devices::drums::id::KICK_TUNE, 48.0),
                    ],
                ]
                .concat(),
                effects: vec![drive(24.0, 1.0), space(0.08)],
                keys: vec![36],
                hold: 0.1,
                length: 1.5,
            },
            with_fx(
                synth("Lead F", "Hyper supersaw", &[65], 0.5, 1.5),
                &[space(0.3)],
            ),
            synth("Reese F", "Raw reese", &[29], 1.0, 1.5),
        ],
        "edm" => [
            vec![
                synth("Bass A", "EDM bass", &[33], 1.0, 1.5),
                with_fx(
                    synth("Big room lead A", "Big room lead", &[57, 69], 0.4, 1.5),
                    &[space(0.2)],
                ),
                with_fx(
                    synth("Pluck F#", "Melodic pluck", &[66], 0.2, 1.5),
                    &[space(0.3)],
                ),
                with_fx(
                    synth("Supersaw chord Am", "Supersaw", &[57, 60, 64], 1.0, 2.0),
                    &[space(0.25)],
                ),
                with_fx(synth("Hard lead A", "Hard lead", &[57], 0.4, 1.0), &[]),
            ],
            kit("Big room", false),
        ]
        .concat(),
        "orchestral" => vec![
            with_fx(
                Sound {
                    params: preset("strings", "Lush strings"),
                    ..inst("Strings Am", "strings", &[57, 60, 64], 2.0, 4.0)
                },
                &[space(0.3)],
            ),
            with_fx(
                Sound {
                    params: preset("pluck", "Harp"),
                    ..inst("Harp C", "pluck", &[60], 0.5, 3.0)
                },
                &[space(0.3)],
            ),
            with_fx(
                Sound {
                    params: preset("pluck", "Pizzicato"),
                    ..inst("Pizzicato C", "pluck", &[60], 0.2, 1.5)
                },
                &[space(0.3)],
            ),
            with_fx(
                Sound {
                    params: preset("organ", "Full organ"),
                    ..inst("Organ C", "organ", &[48, 55, 60, 64], 1.5, 3.0)
                },
                &[space(0.3)],
            ),
        ],
        _ => Vec::new(),
    }
}

/// Renders one sound: hold, release, trim the silent tail.
fn render(s: Sound) -> anyhow::Result<Vec<Vec<f32>>> {
    let ticks_per_sec = BPM / 60.0 * PPQ as f64;
    let hold = (s.hold * ticks_per_sec) as i64;
    let part = Part {
        name: "Sound",
        color: [0, 0, 0],
        trim_db: 0.0,
        instrument: s.instrument,
        params: s.params,
        effects: s.effects,
        notes: s.keys.iter().map(|&k| note(k, 110, 0, hold)).collect(),
    };
    let mut project = assemble(None, BPM, 4, 1, move || vec![part.clone()]);
    project.loop_enabled = false;
    let frames = (s.length * f64::from(SAMPLE_RATE)) as usize;
    let mut out = crate::export::render_project(
        &project,
        &AssetStore::default(),
        &crate::engine_graph::PluginNodes::default(),
        0,
        frames,
        &|_| {},
        &AtomicBool::new(false),
    )
    .ok_or_else(|| anyhow::anyhow!("rendering the sound failed"))?;
    // Trim where it falls below −80 dB, with a 5 ms fade.
    let end = (0..frames)
        .rev()
        .find(|&i| out.iter().any(|c| c.get(i).is_some_and(|x| x.abs() > 1e-4)))
        .map_or(0, |i| (i + 240).min(frames));
    for c in &mut out {
        c.truncate(end);
        let n = c.len();
        for (k, x) in c.iter_mut().skip(n.saturating_sub(240)).enumerate() {
            *x *= 1.0 - k as f32 / 240.0;
        }
    }
    Ok(out)
}

/// Makes the packs under `<data_dir>/Sounds` unless they are up to date; returns that
/// folder.
pub fn install(data_dir: &Path) -> anyhow::Result<PathBuf> {
    let root = data_dir.join("Sounds");
    let marker = root.join(".version");
    if std::fs::read_to_string(&marker).is_ok_and(|v| v.trim() == PACK_VERSION) {
        return Ok(root);
    }
    for genre in musiton_devices::genres::GENRES {
        let dir = root.join(folder(genre));
        std::fs::create_dir_all(&dir)?;
        for s in sounds(genre.id) {
            let path = dir.join(format!("{}.wav", s.name));
            let audio = render(s)?;
            let opts = ExportOptions {
                format: ExportFormat::Wav24,
                normalize: true,
                dither: false,
            };
            write_export(&path, &audio, SAMPLE_RATE, opts)?;
        }
    }
    std::fs::write(&marker, PACK_VERSION)?;
    Ok(root)
}

/// Folder of genre `id`'s pack under `root` (the root itself for no genre).
pub fn genre_dir(root: &Path, id: Option<&str>) -> PathBuf {
    id.and_then(musiton_devices::genres::genre)
        .map_or_else(|| root.to_path_buf(), |g| root.join(folder(g)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_genre_has_sounds_that_sound() {
        for genre in musiton_devices::genres::GENRES {
            let list = sounds(genre.id);
            assert!(list.len() >= 3, "{}: {} sounds", genre.id, list.len());
            for s in list {
                let name = s.name;
                let out = render(s).unwrap();
                let peak = out.iter().flatten().fold(0.0_f32, |m, x| m.max(x.abs()));
                assert!(peak > 0.01, "{} / {name}: silent", genre.id);
                assert!(out[0].len() > 480, "{} / {name}: too short", genre.id);
            }
        }
    }

    #[test]
    fn install_writes_wavs_once() {
        let dir = std::env::temp_dir().join(format!("musiton-sounds-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = install(&dir).unwrap();
        let phonk = genre_dir(&root, Some("phonk"));
        let cowbell = phonk.join("Cowbell F#.wav");
        assert!(cowbell.exists());
        let made = std::fs::metadata(&cowbell).unwrap().modified().unwrap();
        install(&dir).unwrap();
        assert_eq!(
            std::fs::metadata(&cowbell).unwrap().modified().unwrap(),
            made
        );
        assert!(genre_dir(&root, Some("trap")).ends_with("Trap & hip-hop"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
