//! Genre templates (File → New from template): a ready project per genre with tracks,
//! instruments, effects, tempo and a short example of beat and melody. Everything is
//! generated from the built-in instruments, so no audio files are needed. The demo song is
//! built the same way.

use musiton_core::ids::{ClipId, DeviceId, TrackId};
use musiton_core::{TempoMap, Tick, PPQ};
use musiton_devices::drums::id as dr;
use musiton_devices::effects::{compressor, delay, eq, limiter, reverb, saturation, utility};
use musiton_project::{
    AutoMode, AutoPoint, AutomationLane, ChannelStrip, Clip, DeviceSlot, MidiClip, Note,
    OutputRoute, ParamTarget, Project, Track, TrackKind,
};

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
#[derive(Clone)]
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
        description: "Plain saws in overtone-series chords over a deep sine (Aloboi style), gated sixteenths at 120 BPM",
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
    Template {
        name: "Epic orchestral",
        description: "Strings, horns, timpani and harp (90 BPM, D minor). Uses the VSCO-2 orchestra from Options → Sound library when installed, built-in sounds otherwise",
        build: orchestral,
    },
    Template {
        name: "EDM melodic",
        description: "Full song (intro, build-up, drop, break, build-up, drop, outro) in the style of Avicii: piano, pluck lead, supersaw chords, pumping bass (128 BPM, F# minor)",
        build: edm_melodic,
    },
    Template {
        name: "EDM big room",
        description: "Full song with snare-roll build-ups, noise riser, big room lead and pumping bass (140 BPM, A minor)",
        build: edm_big_room,
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

/// Builds a looping project in genre environment `genre` from `parts` (`copies` > 1 repeats every part, for the
/// performance test).
pub fn assemble(
    genre: Option<&str>,
    bpm: f64,
    bars: i64,
    copies: usize,
    parts: impl Fn() -> Vec<Part>,
) -> Project {
    let mut project = Project {
        genre: genre.map(str::to_owned),
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

/// Raw chord velocity (−3 per semitone above B1): the low notes loudest, as in the reference (root and fifth carry
/// the sound, the upper overtones colour it).
fn chord_velocity(pitch: u8) -> u8 {
    (127 - (i32::from(pitch) - 35) * 3).clamp(50, 127) as u8
}

fn raw() -> Project {
    assemble(Some("raw"), 120.0, 4, 1, || {
        // Voicings measured from the owner's recording of Aloboi's "Want To Love (Just
        // Raw)": overtone stacks from the second octave up to about 1 kHz (9/8, 5/4,
        // 27/16, 7/4, 15/8 over the root), with a sine an octave below the root as loud
        // as the chord. B – C# – D# – C#.
        const CHORDS: [&[u8]; 4] = [
            &[35, 42, 51, 58, 61, 63, 70],
            &[37, 44, 51, 58, 63, 65, 71],
            &[39, 46, 51, 55, 58, 63, 74],
            &[37, 44, 51, 58, 63, 65, 71],
        ];
        let chord_of = |bar: i64| CHORDS.get(bar as usize % 4).copied().unwrap_or(&[]);
        // Gated sixteenths: about 80 ms on, 40 ms off, some held over two steps.
        let gate: [(i64, i64); 12] = [
            (0, 1),
            (1, 1),
            (2, 2),
            (4, 1),
            (5, 1),
            (6, 2),
            (8, 1),
            (9, 1),
            (10, 2),
            (12, 1),
            (13, 1),
            (14, 2),
        ];
        let stabs = (0..4)
            .flat_map(|bar| {
                gate.into_iter().flat_map(move |(s, len)| {
                    let length = len * STEP - STEP / 3;
                    chord_of(bar)
                        .iter()
                        .map(move |&p| note(p, chord_velocity(p), bar * BAR + s * STEP, length))
                })
            })
            .collect();
        vec![
            Part {
                name: "Raw stab",
                color: [120, 220, 90],
                trim_db: -1.0,
                instrument: musiton_devices::POLYSYNTH,
                params: preset(musiton_devices::POLYSYNTH, "Raw stab"),
                // About 1.5 s of room, the only stereo in the reference.
                effects: vec![(
                    "reverb",
                    vec![(reverb::id::MIX, 0.35), (reverb::id::DECAY, 1.5)],
                )],
                notes: stabs,
            },
            Part {
                name: "Sub",
                color: [90, 170, 230],
                trim_db: -4.0,
                instrument: musiton_devices::POLYSYNTH,
                params: preset(musiton_devices::POLYSYNTH, "Sine sub"),
                effects: Vec::new(),
                notes: seq(4, |bar| {
                    let root = chord_of(bar).first().copied().unwrap_or(36);
                    vec![(0, root - 12, 16, 100)]
                }),
            },
            drums(
                "Drums",
                "Punchy",
                [
                    hits(4, key::KICK, 120, &[0, 10], 1),
                    hits(4, key::SNARE, 110, &[8], 1),
                    hits(4, key::HAT, 70, &[2, 6, 10, 14], 1),
                ]
                .concat(),
                8.0,
            ),
        ]
    })
}

fn phonk() -> Project {
    assemble(Some("phonk"), 130.0, 4, 1, || {
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
    assemble(Some("tekk"), 160.0, 4, 1, || {
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
    assemble(Some("trap"), 140.0, 4, 1, || {
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
    assemble(Some("house"), 124.0, 4, 1, || {
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
    assemble(Some("techno"), 132.0, 4, 1, || {
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
    assemble(Some("hyperpop"), 160.0, 4, 1, || {
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
    assemble(Some("hardstyle"), 150.0, 4, 1, || {
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

/// An instrument of an installed library: the `.sfz` whose file name is `file`.
fn library_sfz(library: &str, file: &str) -> Option<String> {
    let data = crate::settings::Settings::data_dir()?;
    let dir = crate::library::installed_dir(&data, library)?;
    crate::library::instruments(&dir)
        .into_iter()
        .find(|(_, p)| p.file_name().is_some_and(|n| n == file))
        .map(|(_, p)| p.to_string_lossy().into_owned())
}

/// A part that plays `sfz` from a library when it is installed, else `fallback`.
fn sampled(sfz: Option<&String>, fallback: Part) -> Part {
    match sfz {
        Some(_) => Part {
            instrument: musiton_devices::SFZ,
            params: Vec::new(),
            ..fallback
        },
        None => fallback,
    }
}

fn orchestral() -> Project {
    // Dm – Bb – F – C.
    const ROOTS: [u8; 4] = [50, 46, 53, 48];
    let root = |bar: i64| ROOTS[bar as usize % 4];
    let minor = |bar: i64| bar % 4 == 0;
    let files = [
        "ViolinEnsSusVib.sfz",
        "CelloEnsSusVib.sfz",
        "ContrabassSusVB.sfz",
        "FHornSus.sfz",
        "Timpani.sfz",
        "Harp.sfz",
    ];
    let sfz: Vec<Option<String>> = files.iter().map(|f| library_sfz("vsco2", f)).collect();
    let pick = move |i: usize| sfz.get(i).cloned().flatten();
    let paths: Vec<Option<String>> = (0..files.len()).map(&pick).collect();
    let mut project = assemble(Some("orchestral"), 90.0, 4, 1, move || {
        let chord = |bar: i64, octave: u8| {
            let r = root(bar) + octave;
            let third = if minor(bar) { 3 } else { 4 };
            vec![r, r + third, r + 7]
        };
        let strings = |name, preset_name: &str, notes| Part {
            name,
            color: [200, 150, 90],
            trim_db: 0.0,
            instrument: "strings",
            params: preset("strings", preset_name),
            effects: vec![reverb_fx(0.3)],
            notes,
        };
        vec![
            sampled(
                pick(0).as_ref(),
                strings(
                    "Violins",
                    "Bright violins",
                    seq(4, |bar| {
                        chord(bar, 12).into_iter().map(|p| (0, p, 16, 80)).collect()
                    }),
                ),
            ),
            sampled(
                pick(1).as_ref(),
                strings(
                    "Celli",
                    "Lush strings",
                    seq(4, |bar| {
                        vec![(0, root(bar) - 12, 16, 85), (0, root(bar) - 5, 16, 75)]
                    }),
                ),
            ),
            sampled(
                pick(2).as_ref(),
                strings(
                    "Basses",
                    "Dark pad",
                    seq(4, |bar| vec![(0, root(bar) - 24, 16, 90)]),
                ),
            ),
            sampled(
                pick(3).as_ref(),
                Part {
                    name: "Horns",
                    color: [230, 180, 60],
                    trim_db: -2.0,
                    instrument: "fm",
                    params: preset("fm", "Brass"),
                    effects: vec![reverb_fx(0.3)],
                    notes: seq(4, |bar| {
                        let line: &[(i64, u8, i64)] = match bar % 4 {
                            0 => &[(0, 69, 4), (4, 74, 8), (12, 76, 4)],
                            1 => &[(0, 77, 12), (12, 76, 4)],
                            2 => &[(0, 72, 8), (8, 69, 8)],
                            _ => &[(0, 67, 8), (8, 76, 8)],
                        };
                        line.iter().map(|&(s, p, l)| (s, p, l, 95)).collect()
                    }),
                },
            ),
            sampled(
                pick(4).as_ref(),
                Part {
                    name: "Timpani",
                    color: [220, 90, 80],
                    trim_db: 0.0,
                    instrument: "drums",
                    params: vec![(dr::TOM_TUNE, 70.0), (dr::TOM_DECAY, 1.2)],
                    effects: vec![reverb_fx(0.25)],
                    notes: seq(4, |bar| {
                        let mut hits = vec![(0, 41, 4, 120)];
                        if bar % 2 == 1 {
                            hits.extend([
                                (12, 41, 1, 90),
                                (13, 41, 1, 95),
                                (14, 41, 1, 100),
                                (15, 41, 1, 110),
                            ]);
                        }
                        hits
                    }),
                },
            ),
            sampled(
                pick(5).as_ref(),
                Part {
                    name: "Harp",
                    color: [150, 210, 240],
                    trim_db: 0.0,
                    instrument: "pluck",
                    params: preset("pluck", "Harp"),
                    effects: vec![reverb_fx(0.35)],
                    notes: seq(4, |bar| {
                        let c = chord(bar, 0);
                        let up = [
                            c[0],
                            c[1],
                            c[2],
                            c[0] + 12,
                            c[1] + 12,
                            c[2] + 12,
                            c[0] + 24,
                            c[2] + 12,
                        ];
                        up.into_iter()
                            .enumerate()
                            .map(|(i, p)| (2 * i as i64, p, 2, 80))
                            .collect()
                    }),
                },
            ),
        ]
    });
    // Sampled parts: the timpani sample plays the written pitch (D2), not the tom key.
    for (track, path) in project.tracks.iter_mut().zip(paths) {
        let Some(path) = path else { continue };
        if let Some(musiton_project::DeviceSlot {
            kind: musiton_project::DeviceKind::Builtin { sfz, .. },
            ..
        }) = track.devices.first_mut()
        {
            *sfz = Some(path);
        }
        if track.name == "Timpani" {
            for clip in &mut track.clips {
                if let musiton_project::Clip::Midi(m) = clip {
                    for n in &mut m.notes {
                        n.pitch = 38;
                    }
                }
            }
        }
    }
    project
}

// ---- arranged EDM songs ---------------------------------------------------------------

/// Song sections (bars), as measured in the owner's reference songs: intro 8, build-up 8,
/// drop 16, break 16, build-up 8, drop 16, outro 8.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Intro,
    Build,
    Drop,
    Break,
    Outro,
}

/// Bars of an arranged song.
const SONG_BARS: i64 = 80;

fn section(bar: i64) -> Section {
    match bar {
        0..8 => Section::Intro,
        8..16 | 48..56 => Section::Build,
        16..32 | 56..72 => Section::Drop,
        32..48 => Section::Break,
        _ => Section::Outro,
    }
}

/// First bars of the build-ups and drops.
const BUILDS: [i64; 2] = [8, 48];
const DROPS: [i64; 2] = [16, 56];

/// Snare roll over a build-up: quarters, eighths, sixteenths, then 32nds, getting louder;
/// the last beat before the drop stays empty.
fn snare_roll(key: u8) -> Vec<Note> {
    let mut out = Vec::new();
    for start in BUILDS {
        for b in 0..8 {
            let per_beat: i64 = match b {
                0..4 => 1,
                4..6 => 2,
                6 => 4,
                _ => 8,
            };
            let beats = if b == 7 { 3 } else { 4 };
            for beat in 0..beats {
                for k in 0..per_beat {
                    let len = PPQ / per_beat;
                    let vel = (60 + b * 8 + beat * 2).min(127) as u8;
                    let at = (start + b) * BAR + beat * PPQ + k * len;
                    out.push(note(key, vel, at, len - 5));
                }
            }
        }
    }
    out
}

/// Ids of the track called `name`.
fn track_id(project: &Project, name: &str) -> Option<TrackId> {
    project.tracks.iter().find(|t| t.name == name).map(|t| t.id)
}

/// Pumping: a compressor on each of `targets`, keyed by the track `from` (the kick).
fn sidechain(project: &mut Project, from: &str, targets: &[&str]) {
    let Some(kick) = track_id(project, from) else {
        return;
    };
    for name in targets {
        let id = DeviceId(project.alloc_id());
        let Some(t) = project.tracks.iter_mut().find(|t| t.name == *name) else {
            continue;
        };
        let mut c = DeviceSlot::builtin(id, "compressor");
        for (p, v) in [
            (compressor::id::THRESHOLD, -32.0),
            (compressor::id::RATIO, 10.0),
            (compressor::id::ATTACK, 0.0005),
            (compressor::id::RELEASE, 0.16),
            (compressor::id::SIDECHAIN, 1.0),
        ] {
            c.set_param(p, Some(v));
        }
        c.sidechain = Some(kick);
        // Right after the instrument.
        t.devices.insert(1.min(t.devices.len()), c);
    }
}

/// Automation of parameter `param` of the device at `slot` on track `name`: (bar, value).
fn automate(project: &mut Project, name: &str, slot: usize, param: u64, points: &[(f64, f32)]) {
    let Some(t) = project.tracks.iter_mut().find(|t| t.name == name) else {
        return;
    };
    let Some(d) = t.devices.get(slot) else {
        return;
    };
    let info = musiton_devices::instrument_type(d.type_id())
        .map(|i| i.params)
        .or_else(|| musiton_devices::effect_type(d.type_id()).map(|e| e.params))
        .and_then(|ps| ps.get(param as usize).copied());
    let Some(info) = info else {
        return;
    };
    let target = ParamTarget::Device {
        device: d.id,
        param,
    };
    add_lane(t, target, points, |v| info.to_normalized(v));
}

/// Track volume automation on track `name`: (bar, dB).
fn automate_volume(project: &mut Project, name: &str, points: &[(f64, f32)]) {
    if let Some(t) = project.tracks.iter_mut().find(|t| t.name == name) {
        add_lane(t, ParamTarget::Volume, points, |v| {
            musiton_project::VOLUME_PARAM.to_normalized(v)
        });
    }
}

fn add_lane(t: &mut Track, target: ParamTarget, points: &[(f64, f32)], norm: impl Fn(f32) -> f32) {
    t.automation.push(AutomationLane {
        target,
        points: points
            .iter()
            .map(|&(bar, v)| AutoPoint {
                tick: Tick((bar * BAR as f64) as i64),
                value: norm(v),
                curve: 0.0,
            })
            .collect(),
        mode: AutoMode::Read,
    });
}

/// Riser over both build-ups: cutoff 300 Hz → 12 kHz, volume −30 → 0 dB, silent elsewhere.
fn riser_part() -> Part {
    Part {
        name: "Riser",
        color: [200, 200, 200],
        trim_db: -14.0,
        instrument: musiton_devices::POLYSYNTH,
        params: preset(musiton_devices::POLYSYNTH, "Riser"),
        effects: vec![reverb_fx(0.3)],
        notes: BUILDS
            .iter()
            .map(|&b| note(60, 100, b * BAR, 8 * BAR - 10))
            .collect(),
    }
}

fn riser_automation(project: &mut Project) {
    let mut cut = Vec::new();
    let mut vol = Vec::new();
    for b in BUILDS {
        let b = b as f64;
        cut.extend([(b, 300.0), (b + 8.0, 12_000.0)]);
        vol.extend([(b, -30.0), (b + 7.9, 0.0), (b + 8.0, -60.0)]);
    }
    automate(
        project,
        "Riser",
        0,
        musiton_devices::polysynth::id::CUTOFF,
        &cut,
    );
    automate_volume(project, "Riser", &vol);
}

/// Drum parts of an arranged song: kick (own track, sidechain source), claps, hats,
/// crash and the snare roll.
fn edm_drums(kit: &str) -> Vec<Part> {
    let kick = seq(SONG_BARS, |bar| match section(bar) {
        Section::Drop | Section::Outro => vec![
            (0, key::KICK, 1, 120),
            (4, key::KICK, 1, 120),
            (8, key::KICK, 1, 120),
            (12, key::KICK, 1, 120),
        ],
        Section::Intro if bar >= 4 => vec![
            (0, key::KICK, 1, 110),
            (4, key::KICK, 1, 110),
            (8, key::KICK, 1, 110),
            (12, key::KICK, 1, 110),
        ],
        Section::Build if bar % 40 < 14 => vec![
            (0, key::KICK, 1, 110),
            (4, key::KICK, 1, 110),
            (8, key::KICK, 1, 110),
            (12, key::KICK, 1, 110),
        ],
        _ => Vec::new(),
    });
    let mut top = seq(SONG_BARS, |bar| match section(bar) {
        Section::Drop => {
            let mut v = vec![
                (4, key::CLAP, 1, 115),
                (12, key::CLAP, 1, 115),
                (2, key::OPEN_HAT, 1, 105),
                (6, key::OPEN_HAT, 1, 105),
                (10, key::OPEN_HAT, 1, 105),
                (14, key::OPEN_HAT, 1, 105),
            ];
            // Sixteenth hats, accented off the beat: the top end of the drop.
            v.extend((0..16).map(|s| (s, key::HAT, 1, if s % 2 == 1 { 100 } else { 70 })));
            v
        }
        Section::Outro => (0..16).step_by(2).map(|s| (s, key::HAT, 1, 70)).collect(),
        _ => Vec::new(),
    });
    for b in DROPS.into_iter().chain([32]) {
        top.push(note(key::CRASH, 110, b * BAR, BAR));
    }
    top.extend(snare_roll(key::SNARE));
    vec![
        Part {
            name: "Kick",
            color: [220, 90, 80],
            trim_db: 0.0,
            instrument: "drums",
            params: preset("drums", kit),
            effects: vec![drive_fx(12.0, 0.0)],
            notes: kick,
        },
        Part {
            effects: vec![
                ("compressor", vec![(compressor::id::THRESHOLD, -14.0)]),
                reverb_fx(0.15),
            ],
            ..drums("Drums", kit, top, 6.0)
        },
    ]
}

/// Whole-bar chords for every bar where `play(section)`.
fn chords_in(
    roots: [u8; 4],
    minor: impl Fn(i64) -> bool,
    octave: u8,
    play: impl Fn(Section) -> bool,
) -> Vec<Note> {
    seq(SONG_BARS, |bar| {
        if !play(section(bar)) {
            return Vec::new();
        }
        let r = roots[bar as usize % 4] + octave;
        let third = if minor(bar) { 3 } else { 4 };
        [r, r + third, r + 7]
            .into_iter()
            .map(|p| (0, p, 16, 80))
            .collect()
    })
}

fn edm_melodic() -> Project {
    // F#m – D – A – E.
    const ROOTS: [u8; 4] = [42, 38, 45, 40];
    let minor = |bar: i64| bar % 4 == 0;
    let piano_sfz = library_sfz("salamander", "Salamander Grand Piano V3.sfz");
    let has_piano = piano_sfz.is_some();
    let mut project = assemble(Some("edm"), 128.0, SONG_BARS, 1, move || {
        let root = |bar: i64| ROOTS[bar as usize % 4];
        // Pluck hook (two bars, sixteenths), an original line on the chord tones.
        let hook = |bar: i64| -> Vec<(i64, u8, i64, u8)> {
            let r = root(bar) + 24;
            let third = if minor(bar) { 3 } else { 4 };
            let line: Vec<(i64, u8)> = if bar % 2 == 0 {
                vec![
                    (0, r + 7),
                    (3, r + 12),
                    (6, r + 7),
                    (8, r + third + 12),
                    (10, r + 12),
                    (12, r + 7),
                    (14, r + third),
                ]
            } else {
                vec![
                    (0, r + 12),
                    (3, r + 7),
                    (6, r + third),
                    (8, r + 7),
                    (11, r + 12),
                    (14, r + 14),
                ]
            };
            line.into_iter().map(|(s, p)| (s, p, 2, 100)).collect()
        };
        let piano = Part {
            name: "Piano",
            color: [240, 240, 240],
            trim_db: 0.0,
            instrument: "epiano",
            params: preset("epiano", "Bright"),
            effects: vec![reverb_fx(0.3)],
            notes: seq(SONG_BARS, |bar| {
                if !matches!(section(bar), Section::Intro | Section::Break) {
                    return Vec::new();
                }
                let r = root(bar) + 12;
                let third = if minor(bar) { 3 } else { 4 };
                // Offbeat piano stabs.
                [0, 3, 6, 10, 12]
                    .into_iter()
                    .flat_map(|s| {
                        [r, r + third, r + 7, r + 12]
                            .into_iter()
                            .map(move |p| (s, p, 2, 85))
                    })
                    .collect()
            }),
        };
        let mut parts = edm_drums("Big room");
        parts.extend([
            sampled(piano_sfz.as_ref(), piano),
            Part {
                name: "Pluck",
                color: [120, 220, 240],
                trim_db: 6.0,
                instrument: musiton_devices::POLYSYNTH,
                params: preset(musiton_devices::POLYSYNTH, "Melodic pluck"),
                effects: vec![delay_fx(0.25), reverb_fx(0.25)],
                notes: seq(SONG_BARS, |bar| match section(bar) {
                    Section::Build | Section::Drop => hook(bar),
                    Section::Break if bar >= 40 => hook(bar),
                    _ => Vec::new(),
                }),
            },
            Part {
                name: "Chords",
                color: [240, 120, 220],
                trim_db: 5.0,
                instrument: musiton_devices::POLYSYNTH,
                params: preset(musiton_devices::POLYSYNTH, "Supersaw"),
                effects: vec![reverb_fx(0.25)],
                notes: chords_in(ROOTS, minor, 12, |s| {
                    matches!(s, Section::Drop | Section::Break)
                }),
            },
            Part {
                name: "Bass",
                color: [90, 170, 230],
                trim_db: -6.0,
                instrument: musiton_devices::POLYSYNTH,
                params: preset(musiton_devices::POLYSYNTH, "EDM bass"),
                effects: Vec::new(),
                notes: seq(SONG_BARS, |bar| match section(bar) {
                    Section::Drop | Section::Build => vec![(0, root(bar) - 12, 16, 105)],
                    _ => Vec::new(),
                }),
            },
            riser_part(),
        ]);
        parts
    });
    if has_piano {
        if let Some(path) = library_sfz("salamander", "Salamander Grand Piano V3.sfz") {
            set_sfz(&mut project, "Piano", path);
        }
    }
    sidechain(&mut project, "Kick", &["Bass", "Chords"]);
    riser_automation(&mut project);
    // The pluck opens up through each build-up.
    let mut cut = Vec::new();
    for b in BUILDS {
        let b = b as f64;
        cut.extend([(b, 400.0), (b + 8.0, 2_500.0), (b + 8.01, 900.0)]);
    }
    automate(
        &mut project,
        "Pluck",
        0,
        musiton_devices::polysynth::id::CUTOFF,
        &cut,
    );
    master_bus(&mut project, 20.0);
    project
}

fn edm_big_room() -> Project {
    // Am – F – C – G.
    const ROOTS: [u8; 4] = [45, 41, 48, 43];
    let minor = |bar: i64| bar % 4 == 0;
    let mut project = assemble(Some("edm"), 140.0, SONG_BARS, 1, move || {
        let root = |bar: i64| ROOTS[bar as usize % 4];
        let hook = |bar: i64| -> Vec<(i64, u8, i64, u8)> {
            let r = root(bar) + 12;
            let third = if minor(bar) { 3 } else { 4 };
            let line: Vec<(i64, u8, i64)> = if bar % 2 == 0 {
                vec![
                    (0, r, 2),
                    (2, r, 1),
                    (3, r + third, 2),
                    (6, r, 1),
                    (8, r + 7, 3),
                    (11, r + 5, 2),
                    (14, r + third, 2),
                ]
            } else {
                vec![
                    (0, r + third, 2),
                    (2, r + third, 1),
                    (3, r + 5, 2),
                    (6, r + third, 1),
                    (8, r, 4),
                    (12, r - 2, 2),
                    (14, r, 2),
                ]
            };
            // Big room leads play the hook with its octave.
            line.into_iter()
                .flat_map(|(s, p, l)| [(s, p, l, 110), (s, p + 12, l, 100)])
                .collect()
        };
        let mut parts = edm_drums("Big room");
        parts.extend([
            Part {
                name: "Lead",
                color: [240, 120, 220],
                trim_db: 0.0,
                instrument: musiton_devices::POLYSYNTH,
                params: preset(musiton_devices::POLYSYNTH, "Big room lead"),
                effects: vec![reverb_fx(0.2)],
                notes: seq(SONG_BARS, |bar| match section(bar) {
                    Section::Drop => hook(bar),
                    // Chord stabs on the beat while the filter opens.
                    Section::Build => [0, 4, 8, 12]
                        .into_iter()
                        .flat_map(|s| {
                            let r = root(bar) + 12;
                            let third = if minor(bar) { 3 } else { 4 };
                            [r, r + third, r + 7]
                                .into_iter()
                                .map(move |p| (s, p, 2, 95))
                        })
                        .collect(),
                    _ => Vec::new(),
                }),
            },
            Part {
                name: "Pad",
                color: [150, 110, 220],
                trim_db: 0.0,
                instrument: "strings",
                params: preset("strings", "Lush strings"),
                effects: vec![reverb_fx(0.3)],
                notes: chords_in(ROOTS, minor, 0, |s| {
                    matches!(s, Section::Intro | Section::Break | Section::Outro)
                }),
            },
            Part {
                name: "Bass",
                color: [90, 170, 230],
                trim_db: -5.0,
                instrument: musiton_devices::POLYSYNTH,
                params: preset(musiton_devices::POLYSYNTH, "EDM bass"),
                effects: Vec::new(),
                notes: seq(SONG_BARS, |bar| match section(bar) {
                    Section::Drop => vec![(0, root(bar) - 12, 16, 110)],
                    _ => Vec::new(),
                }),
            },
            riser_part(),
        ]);
        parts
    });
    sidechain(&mut project, "Kick", &["Bass", "Lead", "Pad"]);
    riser_automation(&mut project);
    let mut cut = Vec::new();
    for (b, d) in BUILDS.into_iter().zip(DROPS) {
        cut.extend([
            (b as f64, 500.0),
            (d as f64 - 0.01, 9_000.0),
            (d as f64, 8_000.0),
        ]);
    }
    automate(
        &mut project,
        "Lead",
        0,
        musiton_devices::polysynth::id::CUTOFF,
        &cut,
    );
    master_bus(&mut project, 20.0);
    project
}

/// Mastering like the reference songs (loud and dense): every track goes through a "Master
/// bus" with a gentle glue compressor and a limiter driven by `drive_db`.
fn master_bus(project: &mut Project, drive_db: f32) {
    let id = TrackId(project.alloc_id());
    let mut glue = DeviceSlot::builtin(DeviceId(project.alloc_id()), "compressor");
    for (p, v) in [
        (compressor::id::THRESHOLD, -14.0),
        (compressor::id::RATIO, 2.0),
        (compressor::id::ATTACK, 0.01),
        (compressor::id::RELEASE, 0.2),
    ] {
        glue.set_param(p, Some(v));
    }
    // Brighter, wider top end, as in the reference songs (high shelf on band 7).
    let mut tone = DeviceSlot::builtin(DeviceId(project.alloc_id()), "eq");
    let shelf = 6 * eq::PER_BAND;
    tone.set_param(shelf + 1, Some(5_000.0));
    tone.set_param(shelf + 2, Some(4.0));
    let mut width = DeviceSlot::builtin(DeviceId(project.alloc_id()), "utility");
    width.set_param(utility::id::WIDTH, Some(1.4));
    // A soft clipper rounds off the kick peaks before the limiter (loudness without pumping).
    let mut clip = DeviceSlot::builtin(DeviceId(project.alloc_id()), "saturation");
    for (p, v) in [
        (saturation::id::DRIVE, 6.0),
        (saturation::id::TONE, 20_000.0),
        (saturation::id::OUTPUT, -6.0),
    ] {
        clip.set_param(p, Some(v));
    }
    let mut lim = DeviceSlot::builtin(DeviceId(project.alloc_id()), "limiter");
    lim.set_param(limiter::id::INPUT, Some(drive_db));
    lim.set_param(limiter::id::CEILING, Some(-0.3));
    // A fast release lets the limiter follow the kick instead of ducking the whole beat.
    lim.set_param(limiter::id::RELEASE, Some(0.02));
    for t in &mut project.tracks {
        t.output = OutputRoute::Bus(id);
    }
    project.tracks.push(Track {
        id,
        name: "Master bus".to_owned(),
        color: [120, 120, 120],
        kind: TrackKind::Bus,
        devices: vec![tone, width, glue, clip, lim],
        // Makes up the −3 dB centre pan law of this strip and the master strip, so the
        // limiter's ceiling is the song's peak.
        strip: ChannelStrip {
            gain_db: 6.0,
            ..ChannelStrip::default()
        },
        ..Track::default()
    });
}

/// Makes track `name` play the SFZ instrument at `path`.
fn set_sfz(project: &mut Project, name: &str, path: String) {
    if let Some(DeviceSlot {
        kind: musiton_project::DeviceKind::Builtin { sfz, .. },
        ..
    }) = project
        .tracks
        .iter_mut()
        .find(|t| t.name == name)
        .and_then(|t| t.devices.first_mut())
    {
        *sfz = Some(path);
    }
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
                // Each track from its first note (arranged songs start parts later);
                // soloed, so a master bus it feeds stays audible.
                let Some(first) = track.clips.iter().find_map(|c| match c {
                    Clip::Midi(m) => m.notes.iter().map(|n| n.start.0).min(),
                    _ => None,
                }) else {
                    continue;
                };
                let mut solo = project.clone();
                if let Some(tr) = solo.tracks.get_mut(i) {
                    tr.strip.solo = true;
                }
                let start = project.tempo.tick_to_sample(first as f64, 48_000.0) as u64;
                let out = crate::export::render_project(
                    &solo,
                    &AssetStore::default(),
                    &crate::engine_graph::PluginNodes::default(),
                    start,
                    frames,
                    &|_| {},
                    &AtomicBool::new(false),
                )
                .unwrap();
                let peak = out.iter().flatten().fold(0.0_f32, |m, s| m.max(s.abs()));
                // A riser starts quiet on purpose (its volume is automated upwards).
                let min = if track.automation.is_empty() {
                    0.02
                } else {
                    0.001
                };
                assert!(peak > min, "{} / {}: silent ({peak})", t.name, track.name);
            }
        }
    }

    #[test]
    fn templates_set_a_known_genre() {
        for t in TEMPLATES {
            let g = (t.build)().genre;
            assert!(
                g.as_deref()
                    .and_then(musiton_devices::genres::genre)
                    .is_some(),
                "{}: {g:?}",
                t.name
            );
        }
    }

    #[test]
    fn template_names_are_unique() {
        for (i, a) in TEMPLATES.iter().enumerate() {
            assert!(TEMPLATES[i + 1..].iter().all(|b| b.name != a.name));
        }
    }
}
