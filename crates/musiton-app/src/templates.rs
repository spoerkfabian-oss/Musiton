//! Genre templates (File → New from template): a ready project per genre with tracks,
//! instruments, effects, tempo and a short example of beat and melody. Everything is
//! generated from the built-in instruments, so no audio files are needed. The demo song is
//! built the same way.

use musiton_core::ids::{ClipId, DeviceId, TrackId};
use musiton_core::{TempoMap, Tick, PPQ};
use musiton_devices::drums::id as dr;
use musiton_devices::effects::{compressor, delay, reverb, saturation, utility};
use musiton_project::{Clip, DeviceSlot, MidiClip, Note, Project, Track, TrackKind};

/// One bar of 4/4 in ticks.
pub const BAR: i64 = 4 * PPQ;
/// A sixteenth note in ticks.
pub const STEP: i64 = PPQ / 4;

/// General MIDI drum keys of the built-in drum synth.
mod key {
    pub const KICK: u8 = 36;
    pub const SNARE: u8 = 38;
    pub const CLAP: u8 = 39;
    pub const HAT: u8 = 42;
    pub const OPEN_HAT: u8 = 46;
    pub const CRASH: u8 = 49;
}

/// One track of a template: instrument with settings, effects and notes.
pub struct Part {
    /// Track name.
    pub name: &'static str,
    /// Track colour.
    pub color: [u8; 3],
    /// Level correction (dB) by a utility device at the end of the chain.
    pub trim_db: f32,
    /// Built-in instrument type id.
    pub instrument: &'static str,
    /// Instrument parameter values.
    pub params: Vec<(u64, f32)>,
    /// Effects (type id, parameter values) after the instrument.
    pub effects: Vec<(&'static str, Vec<(u64, f32)>)>,
    /// The example notes.
    pub notes: Vec<Note>,
}

/// A genre template.
pub struct Template {
    /// Name in the menu.
    pub name: &'static str,
    /// One line about the sound (menu tooltip).
    pub description: &'static str,
    /// Builds the project.
    pub build: fn() -> Project,
}

/// All templates, in menu order.
pub const TEMPLATES: &[Template] = &[
    Template {
        name: "Raw",
        description: "Distorted saws in just-intonation chords, chopped at 170 BPM (C# minor)",
        build: raw,
    },
    Template {
        name: "Phonk",
        description: "2022/23 drift phonk: melodic 808 cowbell, distorted 808, Memphis hats (130 BPM, F# minor)",
        build: phonk,
    },
    Template {
        name: "Tekk",
        description: "Hardtekk: distorted four-on-the-floor kick, offbeat bass, high lead (160 BPM, A minor)",
        build: tekk,
    },
    Template {
        name: "Trap",
        description: "Gliding 808, hi-hat rolls, clap on 3, bell melody and pad (140 BPM, C minor)",
        build: trap,
    },
    Template {
        name: "House",
        description: "Four-on-the-floor, offbeat open hats, electric piano chords, bass (124 BPM, A minor)",
        build: house,
    },
    Template {
        name: "Techno",
        description: "Driving kick, acid line, offbeat hats and a dark pad (132 BPM, A minor)",
        build: techno,
    },
    Template {
        name: "Hyperpop",
        description: "Wide supersaw chords, distorted 808, bright pluck lead (160 BPM, E major)",
        build: hyperpop,
    },
    Template {
        name: "Hardstyle",
        description: "Long distorted kick, offbeat bass, euphoric supersaw lead (150 BPM, F minor)",
        build: hardstyle,
    },
];

/// A note `start` and `length` in ticks.
pub fn note(pitch: u8, velocity: u8, start: i64, length: i64) -> Note {
    Note {
        pitch,
        velocity,
        start: Tick(start),
        length: Tick(length),
        channel: 0,
    }
}

/// The same hits in every bar: `steps` in sixteenths, each `len` sixteenths long.
pub fn hits(bars: i64, pitch: u8, velocity: u8, steps: &[i64], len: i64) -> Vec<Note> {
    seq(bars, |_| {
        steps.iter().map(|&s| (s, pitch, len, velocity)).collect()
    })
}

/// Notes bar by bar: `f(bar)` gives (step, pitch, length in steps, velocity).
pub fn seq(bars: i64, f: impl Fn(i64) -> Vec<(i64, u8, i64, u8)>) -> Vec<Note> {
    (0..bars)
        .flat_map(|bar| {
            f(bar).into_iter().map(move |(s, p, len, v)| {
                note(p, v, bar * BAR + s * STEP, (len * STEP - 10).max(10))
            })
        })
        .collect()
}

/// Factory preset `name` of instrument `type_id` (empty if not found).
pub fn preset(type_id: &str, name: &str) -> Vec<(u64, f32)> {
    musiton_devices::instrument_type(type_id)
        .and_then(|t| t.presets.iter().find(|(n, _)| *n == name))
        .map(|(_, p)| p.to_vec())
        .unwrap_or_default()
}

/// Builds a looping project from `parts` (`copies` > 1 repeats every part, for the
/// performance test).
pub fn assemble(bpm: f64, bars: i64, copies: usize, parts: impl Fn() -> Vec<Part>) -> Project {
    let mut project = Project {
        tempo: TempoMap::constant(bpm),
        loop_range: Some((Tick(0), Tick(bars * BAR))),
        loop_enabled: true,
        metronome: false,
        ..Project::default()
    };
    for copy in 0..copies {
        for part in parts() {
            let track_id = TrackId(project.alloc_id());
            let mut instrument = DeviceSlot::builtin(DeviceId(project.alloc_id()), part.instrument);
            for &(p, v) in &part.params {
                instrument.set_param(p, Some(v));
            }
            let mut devices = vec![instrument];
            for (type_id, params) in &part.effects {
                let mut fx = DeviceSlot::builtin(DeviceId(project.alloc_id()), type_id);
                for &(p, v) in params {
                    fx.set_param(p, Some(v));
                }
                devices.push(fx);
            }
            let mut trim = DeviceSlot::builtin(DeviceId(project.alloc_id()), "utility");
            trim.set_param(utility::id::GAIN, Some(part.trim_db));
            devices.push(trim);
            let mut track = Track {
                id: track_id,
                name: if copies == 1 {
                    part.name.to_owned()
                } else {
                    format!("{} {}", part.name, copy + 1)
                },
                color: part.color,
                kind: TrackKind::Instrument,
                devices,
                clips: vec![Clip::Midi(MidiClip {
                    id: ClipId(project.alloc_id()),
                    start: Tick(0),
                    length: Tick(bars * BAR),
                    notes: part.notes,
                    cc: Vec::new(),
                })],
                ..Track::default()
            };
            track.strip.gain_db = -10.0 * (copies as f32).log10();
            project.tracks.push(track);
        }
    }
    project
}

fn with(mut base: Vec<(u64, f32)>, extra: &[(u64, f32)]) -> Vec<(u64, f32)> {
    base.extend_from_slice(extra);
    base
}

fn drums(name: &'static str, preset_name: &str, notes: Vec<Note>, trim_db: f32) -> Part {
    Part {
        name,
        color: [220, 90, 80],
        trim_db,
        instrument: "drums",
        params: preset("drums", preset_name),
        effects: vec![("compressor", vec![(compressor::id::THRESHOLD, -14.0)])],
        notes,
    }
}

fn reverb_fx(mix: f32) -> (&'static str, Vec<(u64, f32)>) {
    ("reverb", vec![(reverb::id::MIX, mix)])
}

fn delay_fx(mix: f32) -> (&'static str, Vec<(u64, f32)>) {
    ("delay", vec![(delay::id::MIX, mix)])
}

/// Saturation with `drive` dB and character (0 soft, 1 hard, 2 tube).
fn drive_fx(drive: f32, kind: f32) -> (&'static str, Vec<(u64, f32)>) {
    (
        "saturation",
        vec![(saturation::id::DRIVE, drive), (saturation::id::KIND, kind)],
    )
}

/// Triad (or power chord with `third` = 0 … 0 means root, fifth, octave).
fn chord(root: u8, third: u8) -> Vec<u8> {
    if third == 0 {
        vec![root, root + 7, root + 12]
    } else {
        vec![root, root + third, root + 7]
    }
}

// ---- templates -------------------------------------------------------------------------

fn raw() -> Project {
    assemble(170.0, 4, 1, || {
        // C#, C#, A, B: power chords the synth tunes pure.
        const ROOTS: [u8; 4] = [37, 37, 33, 35];
        let root = |bar: i64| ROOTS[bar as usize % 4];
        vec![
            Part {
                name: "Raw stab",
                color: [120, 220, 90],
                trim_db: -1.0,
                instrument: musiton_devices::POLYSYNTH,
                params: preset(musiton_devices::POLYSYNTH, "Raw stab"),
                effects: vec![reverb_fx(0.12)],
                notes: seq(4, |bar| {
                    [(0, 2), (2, 2), (4, 4), (8, 2), (10, 2), (12, 1), (14, 2)]
                        .into_iter()
                        .flat_map(|(s, len)| {
                            chord(root(bar), 0)
                                .into_iter()
                                .map(move |p| (s, p, len, 110))
                        })
                        .collect()
                }),
            },
            Part {
                name: "Sub",
                color: [90, 170, 230],
                trim_db: -3.0,
                instrument: musiton_devices::POLYSYNTH,
                params: preset(musiton_devices::POLYSYNTH, "Dirty sub"),
                effects: Vec::new(),
                notes: seq(4, |bar| vec![(0, root(bar) - 12, 16, 100)]),
            },
            drums(
                "Drums",
                "Punchy",
                [
                    hits(4, key::KICK, 120, &[0, 6, 10], 1),
                    hits(4, key::SNARE, 110, &[8], 1),
                    hits(4, key::HAT, 70, &[0, 2, 4, 6, 8, 10, 12, 14], 1),
                ]
                .concat(),
                8.0,
            ),
        ]
    })
}

fn phonk() -> Project {
    assemble(130.0, 4, 1, || {
        // F#m – D – E – C#.
        const ROOTS: [u8; 4] = [30, 26, 28, 25];
        let root = |bar: i64| ROOTS[bar as usize % 4];
        let cowbell = seq(4, |bar| {
            let line: &[(i64, u8)] = if bar % 2 == 0 {
                &[
                    (0, 66),
                    (3, 66),
                    (6, 69),
                    (8, 68),
                    (10, 66),
                    (12, 73),
                    (14, 71),
                ]
            } else {
                &[
                    (0, 66),
                    (3, 66),
                    (6, 69),
                    (8, 71),
                    (10, 69),
                    (12, 67),
                    (14, 66),
                ]
            };
            line.iter().map(|&(s, p)| (s, p, 2, 110)).collect()
        });
        // Memphis hats: eighths, a triplet roll at the end of every second bar.
        let mut hats = hits(4, key::HAT, 75, &[0, 2, 4, 6, 8, 10, 12], 1);
        for bar in 0..4 {
            let at = bar * BAR + 14 * STEP;
            let rolls = if bar % 2 == 1 { 6 } else { 2 };
            for k in 0..rolls {
                let len = 2 * STEP / rolls;
                hats.push(note(key::HAT, 60 + 8 * k as u8, at + k * len, len - 5));
            }
        }
        vec![
            Part {
                name: "Cowbell",
                color: [240, 200, 60],
                trim_db: 5.0,
                instrument: musiton_devices::POLYSYNTH,
                params: preset(musiton_devices::POLYSYNTH, "Phonk cowbell"),
                effects: vec![drive_fx(10.0, 1.0), reverb_fx(0.25)],
                notes: cowbell,
            },
            Part {
                name: "808",
                color: [90, 170, 230],
                trim_db: -7.0,
                instrument: musiton_devices::POLYSYNTH,
                params: preset(musiton_devices::POLYSYNTH, "Phonk 808"),
                effects: vec![drive_fx(14.0, 2.0)],
                notes: seq(4, |bar| {
                    let r = root(bar) + 12;
                    vec![(0, r, 5, 115), (6, r, 3, 110), (10, r + 12, 4, 105)]
                }),
            },
            drums(
                "Drums",
                "Phonk",
                [
                    hits(4, key::KICK, 120, &[0, 7, 10], 1),
                    hits(4, key::CLAP, 110, &[4, 12], 1),
                    hits(4, key::SNARE, 90, &[4, 12], 1),
                    hats,
                ]
                .concat(),
                4.0,
            ),
        ]
    })
}

fn tekk() -> Project {
    assemble(160.0, 4, 1, || {
        // Am – F – C – G.
        const ROOTS: [u8; 4] = [45, 41, 48, 43];
        let root = |bar: i64| ROOTS[bar as usize % 4];
        vec![
            Part {
                name: "Lead",
                color: [230, 120, 200],
                trim_db: 5.0,
                instrument: musiton_devices::POLYSYNTH,
                params: preset(musiton_devices::POLYSYNTH, "Tekk lead"),
                effects: vec![delay_fx(0.2), reverb_fx(0.2)],
                notes: seq(4, |bar| {
                    let r = root(bar) + 24;
                    let third = if bar % 4 == 0 { 3 } else { 4 };
                    [
                        (0, r + 7),
                        (2, r + 12),
                        (4, r + 7),
                        (6, r + third),
                        (8, r),
                        (10, r + third),
                        (12, r + 7),
                        (14, r + 12),
                    ]
                    .into_iter()
                    .map(|(s, p)| (s, p, 2, 100))
                    .collect()
                }),
            },
            Part {
                name: "Bass",
                color: [90, 170, 230],
                trim_db: 3.0,
                instrument: musiton_devices::POLYSYNTH,
                params: preset(musiton_devices::POLYSYNTH, "Bass"),
                effects: vec![drive_fx(8.0, 0.0)],
                notes: seq(4, |bar| {
                    let r = root(bar) - 12;
                    [2, 6, 10, 14].into_iter().map(|s| (s, r, 2, 110)).collect()
                }),
            },
            Part {
                name: "Kick",
                color: [220, 90, 80],
                trim_db: 0.0,
                instrument: "drums",
                params: preset("drums", "Tekk"),
                effects: vec![drive_fx(18.0, 1.0)],
                notes: hits(4, key::KICK, 127, &[0, 4, 8, 12], 1),
            },
            drums(
                "Drums",
                "Tekk",
                [
                    hits(4, key::CLAP, 105, &[4, 12], 1),
                    hits(4, key::OPEN_HAT, 80, &[2, 6, 10, 14], 1),
                    hits(4, key::HAT, 60, &[1, 3, 5, 7, 9, 11, 13, 15], 1),
                ]
                .concat(),
                7.0,
            ),
        ]
    })
}

fn trap() -> Project {
    assemble(140.0, 4, 1, || {
        // Cm – Ab – Eb – Bb.
        const ROOTS: [u8; 4] = [48, 44, 51, 46];
        let root = |bar: i64| ROOTS[bar as usize % 4];
        let mut hats = hits(4, key::HAT, 70, &[0, 2, 4, 6, 8, 10], 1);
        for bar in 0..4 {
            // A 32nd-note roll into the next bar.
            for k in 0..8 {
                hats.push(note(
                    key::HAT,
                    55 + 6 * k as u8,
                    bar * BAR + 12 * STEP + k * STEP / 2,
                    STEP / 2 - 5,
                ));
            }
        }
        vec![
            Part {
                name: "Bells",
                color: [150, 210, 240],
                trim_db: 14.0,
                instrument: musiton_devices::POLYSYNTH,
                params: preset(musiton_devices::POLYSYNTH, "Pluck"),
                effects: vec![delay_fx(0.3), reverb_fx(0.3)],
                notes: seq(4, |bar| {
                    let r = root(bar) + 12;
                    let third = if bar % 4 == 0 { 3 } else { 4 };
                    [
                        (0, r + 12),
                        (3, r + 7),
                        (6, r + third + 12),
                        (10, r + 7),
                        (12, r + 12),
                    ]
                    .into_iter()
                    .map(|(s, p)| (s, p, 2, 95))
                    .collect()
                }),
            },
            Part {
                name: "Pad",
                color: [150, 110, 220],
                trim_db: 4.0,
                instrument: "strings",
                params: preset("strings", "Dark pad"),
                effects: vec![reverb_fx(0.3)],
                notes: seq(4, |bar| {
                    let third = if bar % 4 == 0 { 3 } else { 4 };
                    chord(root(bar), third)
                        .into_iter()
                        .map(|p| (0, p, 16, 70))
                        .collect()
                }),
            },
            Part {
                name: "808",
                color: [90, 170, 230],
                trim_db: -1.0,
                instrument: musiton_devices::POLYSYNTH,
                params: preset(musiton_devices::POLYSYNTH, "Phonk 808"),
                effects: vec![drive_fx(8.0, 0.0)],
                notes: seq(4, |bar| {
                    let r = root(bar) - 24;
                    vec![
                        (0, r, 6, 115),
                        (7, r, 3, 110),
                        (11, r + 12, 2, 105),
                        (14, r, 2, 110),
                    ]
                }),
            },
            drums(
                "Drums",
                "808 style",
                [
                    hits(4, key::KICK, 120, &[0, 7, 11], 1),
                    hits(4, key::CLAP, 115, &[8], 1),
                    hats,
                ]
                .concat(),
                4.0,
            ),
        ]
    })
}

fn house() -> Project {
    assemble(124.0, 4, 1, || {
        // Am7 – Fmaj7 – Cmaj7 – G (as seventh chords).
        const ROOTS: [u8; 4] = [57, 53, 48, 55];
        let root = |bar: i64| ROOTS[bar as usize % 4];
        vec![
            Part {
                name: "Keys",
                color: [240, 170, 90],
                trim_db: 4.0,
                instrument: "epiano",
                params: Vec::new(),
                effects: vec![reverb_fx(0.2)],
                notes: seq(4, |bar| {
                    let r = root(bar);
                    let third = if bar % 4 == 0 { 3 } else { 4 };
                    let seventh = if bar % 4 == 0 || bar % 4 == 3 { 10 } else { 11 };
                    [3, 6, 11]
                        .into_iter()
                        .flat_map(|s| {
                            [r, r + third, r + 7, r + seventh]
                                .into_iter()
                                .map(move |p| (s, p, 2, 90))
                        })
                        .collect()
                }),
            },
            Part {
                name: "Bass",
                color: [90, 170, 230],
                trim_db: 5.0,
                instrument: musiton_devices::POLYSYNTH,
                params: preset(musiton_devices::POLYSYNTH, "Bass"),
                effects: Vec::new(),
                notes: seq(4, |bar| {
                    let r = root(bar) - 24;
                    [(2, r), (6, r), (10, r + 12), (14, r)]
                        .into_iter()
                        .map(|(s, p)| (s, p, 2, 105))
                        .collect()
                }),
            },
            drums(
                "Drums",
                "House",
                [
                    hits(4, key::KICK, 120, &[0, 4, 8, 12], 1),
                    hits(4, key::CLAP, 100, &[4, 12], 1),
                    hits(4, key::OPEN_HAT, 85, &[2, 6, 10, 14], 1),
                    hits(
                        4,
                        key::HAT,
                        55,
                        &[0, 1, 3, 4, 5, 7, 8, 9, 11, 12, 13, 15],
                        1,
                    ),
                ]
                .concat(),
                5.0,
            ),
        ]
    })
}

fn techno() -> Project {
    assemble(132.0, 4, 1, || {
        vec![
            Part {
                name: "Acid",
                color: [240, 220, 80],
                trim_db: 1.0,
                instrument: "acid",
                params: Vec::new(),
                effects: vec![delay_fx(0.2)],
                notes: seq(4, |bar| {
                    let line: [(i64, u8); 8] = if bar % 2 == 0 {
                        [
                            (0, 33),
                            (2, 45),
                            (3, 33),
                            (6, 36),
                            (8, 33),
                            (10, 43),
                            (12, 33),
                            (14, 45),
                        ]
                    } else {
                        [
                            (0, 33),
                            (2, 45),
                            (4, 40),
                            (6, 33),
                            (8, 48),
                            (10, 33),
                            (12, 43),
                            (13, 45),
                        ]
                    };
                    line.into_iter().map(|(s, p)| (s, p, 1, 100)).collect()
                }),
            },
            Part {
                name: "Pad",
                color: [150, 110, 220],
                trim_db: 4.0,
                instrument: "strings",
                params: preset("strings", "Dark pad"),
                effects: vec![reverb_fx(0.35)],
                notes: seq(4, |bar| {
                    let r = if bar % 2 == 0 { 57 } else { 53 };
                    chord(r, if bar % 2 == 0 { 3 } else { 4 })
                        .into_iter()
                        .map(|p| (0, p, 16, 60))
                        .collect()
                }),
            },
            drums(
                "Drums",
                "Punchy",
                [
                    hits(4, key::KICK, 125, &[0, 4, 8, 12], 1),
                    hits(4, key::CLAP, 80, &[4, 12], 1),
                    hits(4, key::OPEN_HAT, 80, &[2, 6, 10, 14], 1),
                    hits(4, key::HAT, 50, &[1, 3, 5, 7, 9, 11, 13, 15], 1),
                ]
                .concat(),
                7.0,
            ),
        ]
    })
}

fn hyperpop() -> Project {
    assemble(160.0, 4, 1, || {
        // E – B – C#m – A.
        const ROOTS: [u8; 4] = [52, 47, 49, 45];
        let root = |bar: i64| ROOTS[bar as usize % 4];
        let minor = |bar: i64| bar % 4 == 2;
        vec![
            Part {
                name: "Supersaw",
                color: [240, 120, 220],
                trim_db: 0.0,
                instrument: musiton_devices::POLYSYNTH,
                params: preset(musiton_devices::POLYSYNTH, "Hyper supersaw"),
                effects: vec![reverb_fx(0.25)],
                notes: seq(4, |bar| {
                    let third = if minor(bar) { 3 } else { 4 };
                    [0, 3, 6, 8, 11, 14]
                        .into_iter()
                        .flat_map(|s| {
                            chord(root(bar) + 12, third)
                                .into_iter()
                                .map(move |p| (s, p, 2, 95))
                        })
                        .collect()
                }),
            },
            Part {
                name: "Lead",
                color: [150, 230, 250],
                trim_db: 12.0,
                instrument: "pluck",
                params: Vec::new(),
                effects: vec![delay_fx(0.25)],
                notes: seq(4, |bar| {
                    let r = root(bar) + 24;
                    let third = if minor(bar) { 3 } else { 4 };
                    [
                        (0, r + 12),
                        (2, r + 7),
                        (4, r + third),
                        (6, r + 7),
                        (8, r + 12),
                        (10, r + 14),
                        (12, r + 12),
                        (14, r + 7),
                    ]
                    .into_iter()
                    .map(|(s, p)| (s, p, 2, 100))
                    .collect()
                }),
            },
            Part {
                name: "808",
                color: [90, 170, 230],
                trim_db: -8.0,
                instrument: musiton_devices::POLYSYNTH,
                params: preset(musiton_devices::POLYSYNTH, "Phonk 808"),
                effects: vec![drive_fx(16.0, 1.0)],
                notes: seq(4, |bar| {
                    let r = root(bar) - 24;
                    vec![(0, r, 6, 115), (8, r, 4, 110), (12, r + 12, 4, 105)]
                }),
            },
            drums(
                "Drums",
                "Punchy",
                [
                    hits(4, key::KICK, 120, &[0, 8, 10], 1),
                    hits(4, key::SNARE, 110, &[4, 12], 1),
                    hits(
                        4,
                        key::HAT,
                        65,
                        &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
                        1,
                    ),
                    hits(1, key::CRASH, 90, &[0], 8),
                ]
                .concat(),
                6.0,
            ),
        ]
    })
}

fn hardstyle() -> Project {
    assemble(150.0, 4, 1, || {
        // Fm – Db – Ab – Eb.
        const ROOTS: [u8; 4] = [41, 37, 44, 39];
        let root = |bar: i64| ROOTS[bar as usize % 4];
        vec![
            Part {
                name: "Lead",
                color: [240, 120, 220],
                trim_db: 6.0,
                instrument: musiton_devices::POLYSYNTH,
                params: preset(musiton_devices::POLYSYNTH, "Hyper supersaw"),
                effects: vec![delay_fx(0.15), reverb_fx(0.3)],
                notes: seq(4, |bar| {
                    let r = root(bar) + 24;
                    let third = if bar % 4 == 0 { 3 } else { 4 };
                    [
                        (0, r + 12, 3),
                        (3, r + 7, 3),
                        (6, r + third + 12, 2),
                        (8, r + 12, 4),
                        (12, r + 7, 4),
                    ]
                    .into_iter()
                    .map(|(s, p, len)| (s, p, len, 100))
                    .collect()
                }),
            },
            Part {
                name: "Bass",
                color: [90, 170, 230],
                trim_db: -2.0,
                instrument: musiton_devices::POLYSYNTH,
                params: preset(musiton_devices::POLYSYNTH, "Raw reese"),
                effects: Vec::new(),
                notes: seq(4, |bar| {
                    let r = root(bar) - 12;
                    [2, 6, 10, 14].into_iter().map(|s| (s, r, 2, 110)).collect()
                }),
            },
            Part {
                name: "Kick",
                color: [220, 90, 80],
                trim_db: -4.0,
                instrument: "drums",
                params: with(
                    preset("drums", "Tekk"),
                    &[(dr::KICK_DECAY, 0.6), (dr::KICK_TUNE, 48.0)],
                ),
                effects: vec![drive_fx(24.0, 1.0), reverb_fx(0.08)],
                notes: hits(4, key::KICK, 127, &[0, 4, 8, 12], 1),
            },
            drums(
                "Drums",
                "Punchy",
                [
                    hits(4, key::CLAP, 100, &[4, 12], 1),
                    hits(4, key::OPEN_HAT, 75, &[2, 6, 10, 14], 1),
                ]
                .concat(),
                11.0,
            ),
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::AssetStore;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn every_template_plays_every_track_without_clipping() {
        for t in TEMPLATES {
            let project = (t.build)();
            let frames = 48_000 * 4;
            let mix = crate::export::render_project(
                &project,
                &AssetStore::default(),
                &crate::engine_graph::PluginNodes::default(),
                0,
                frames,
                &|_| {},
                &AtomicBool::new(false),
            )
            .unwrap();
            let peak = mix.iter().flatten().fold(0.0_f32, |m, s| m.max(s.abs()));
            assert!(peak > 0.1 && peak < 1.0, "{}: peak {peak}", t.name);
            for (i, track) in project.tracks.iter().enumerate() {
                let mut solo = project.clone();
                for (k, tr) in solo.tracks.iter_mut().enumerate() {
                    tr.strip.mute = k != i;
                }
                let out = crate::export::render_project(
                    &solo,
                    &AssetStore::default(),
                    &crate::engine_graph::PluginNodes::default(),
                    0,
                    frames,
                    &|_| {},
                    &AtomicBool::new(false),
                )
                .unwrap();
                let peak = out.iter().flatten().fold(0.0_f32, |m, s| m.max(s.abs()));
                assert!(peak > 0.02, "{} / {}: silent ({peak})", t.name, track.name);
            }
        }
    }

    #[test]
    fn template_names_are_unique() {
        for (i, a) in TEMPLATES.iter().enumerate() {
            assert!(TEMPLATES[i + 1..].iter().all(|b| b.name != a.name));
        }
    }
}
