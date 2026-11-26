//! "Raw song": an original 80-bar song (about 2:40) in the owner-approved Raw sound, in
//! D major with chords borrowed from D minor; longing and dark. Harmony, melody and
//! arrangement were designed by two music-theory passes (see NOTES.md, "Raw song").
//! Form: intro 8, verse 16, build 8, drop 16, break 8, drop 2 16, outro 8 bars.

use super::{
    assemble, automate, automate_volume, chord_velocity, drums, key, master_bus, note, preset,
    raw_chord_part, raw_sub_part, Note, Part, Project, BAR, STEP,
};
use musiton_devices::effects::{delay, reverb};
use musiton_devices::polysynth::id as ps;

// ---- harmony and melody --------------------------------------------------------------

/// Chord voicings: overtone stacks over a root in A1–E2 (each chord is tuned to its own
/// root by the synth).
const D_MAJ9: &[u8] = &[38, 45, 52, 61, 66, 69, 76]; // Dmaj9
const D_M9: &[u8] = &[38, 45, 52, 60, 65, 69, 76]; // Dm9
const A_7: &[u8] = &[33, 40, 47, 55, 61, 64, 71]; // A7
const A_7SUS2: &[u8] = &[33, 40, 47, 55, 59, 64, 71]; // A7sus2
const BB_MAJ9: &[u8] = &[34, 41, 48, 57, 62, 65, 72]; // Bbmaj9
const C_9: &[u8] = &[36, 43, 50, 58, 64, 67, 74]; // C9
const B_M9: &[u8] = &[35, 42, 49, 57, 62, 66, 73]; // Bm9
const E_9: &[u8] = &[40, 47, 54, 62, 68, 71, 76]; // E9
const EB_MAJ9_S11: &[u8] = &[39, 46, 53, 62, 67, 69, 74]; // Ebmaj9#11

/// The chords one after another: (voicing, sixteenths).
const HARMONY: &[(&[u8], i64)] = &[
    // Intro (8 bars)
    (D_MAJ9, 32),
    (BB_MAJ9, 32),
    (B_M9, 32),
    (A_7SUS2, 16),
    (A_7, 16),
    // Verse (16 bars)
    (D_M9, 24),
    (C_9, 8),
    (BB_MAJ9, 32),
    (B_M9, 24),
    (E_9, 8),
    (EB_MAJ9_S11, 16),
    (A_7, 16),
    (D_MAJ9, 24),
    (C_9, 8),
    (BB_MAJ9, 32),
    (B_M9, 24),
    (E_9, 8),
    (EB_MAJ9_S11, 24),
    (A_7, 8),
    // Build (8 bars)
    (BB_MAJ9, 16),
    (C_9, 16),
    (B_M9, 16),
    (E_9, 16),
    (EB_MAJ9_S11, 8),
    (E_9, 8),
    (EB_MAJ9_S11, 8),
    (E_9, 8),
    (A_7SUS2, 16),
    (A_7, 16),
    // Drop (16 bars)
    (D_MAJ9, 24),
    (A_7, 8),
    (BB_MAJ9, 24),
    (C_9, 8),
    (B_M9, 24),
    (E_9, 8),
    (EB_MAJ9_S11, 24),
    (A_7, 8),
    (D_MAJ9, 24),
    (A_7, 8),
    (BB_MAJ9, 24),
    (C_9, 8),
    (B_M9, 24),
    (E_9, 8),
    (EB_MAJ9_S11, 24),
    (A_7, 8),
    // Break (8 bars)
    (D_M9, 32),
    (BB_MAJ9, 32),
    (EB_MAJ9_S11, 32),
    (A_7SUS2, 32),
    // Drop 2 (16 bars)
    (D_MAJ9, 24),
    (A_7, 8),
    (BB_MAJ9, 24),
    (C_9, 8),
    (D_M9, 24),
    (E_9, 8),
    (EB_MAJ9_S11, 24),
    (A_7, 8),
    (D_MAJ9, 24),
    (A_7, 8),
    (BB_MAJ9, 24),
    (C_9, 8),
    (B_M9, 16),
    (BB_MAJ9, 8),
    (C_9, 8),
    (D_MAJ9, 32),
    // Outro (8 bars)
    (D_MAJ9, 32),
    (BB_MAJ9, 32),
    (EB_MAJ9_S11, 32),
    (D_MAJ9, 32),
];

/// Lead melodies: (sixteenth from the section start, length, pitch, velocity, slide).
const DROP_LEAD: &[(i64, i64, u8, u8, bool)] = &[
    (0, 3, 76, 100, false),
    (3, 3, 78, 96, false),
    (6, 10, 81, 112, true),
    (18, 4, 78, 90, false),
    (24, 6, 79, 105, false),
    (32, 3, 74, 100, false),
    (35, 3, 77, 96, false),
    (38, 10, 81, 110, false),
    (50, 4, 77, 90, false),
    (56, 4, 79, 100, false),
    (60, 4, 76, 85, true),
    (64, 3, 81, 105, false),
    (67, 3, 78, 95, false),
    (70, 10, 74, 100, false),
    (82, 4, 73, 85, false),
    (88, 3, 76, 95, false),
    (91, 5, 80, 108, false),
    (96, 14, 79, 110, true),
    (112, 4, 74, 85, false),
    (116, 4, 70, 80, false),
    (120, 6, 73, 95, false),
    (128, 3, 76, 104, false),
    (131, 3, 78, 100, false),
    (134, 10, 85, 118, false),
    (146, 3, 81, 96, false),
    (149, 3, 78, 92, false),
    (152, 6, 79, 108, false),
    (160, 3, 74, 104, false),
    (163, 3, 77, 100, false),
    (166, 10, 84, 115, false),
    (178, 4, 81, 94, false),
    (184, 4, 82, 104, false),
    (188, 4, 79, 88, true),
    (192, 3, 81, 108, false),
    (195, 3, 78, 98, false),
    (198, 10, 74, 104, false),
    (210, 4, 71, 88, false),
    (216, 3, 76, 98, false),
    (219, 5, 80, 110, false),
    (224, 14, 79, 112, true),
    (240, 4, 77, 90, false),
    (244, 4, 74, 86, false),
    (248, 6, 69, 90, true),
];
const DROP2_LEAD: &[(i64, i64, u8, u8, bool)] = &[
    (0, 3, 76, 100, false),
    (3, 3, 78, 96, false),
    (6, 10, 81, 112, true),
    (18, 4, 78, 90, false),
    (24, 6, 79, 105, false),
    (32, 3, 74, 100, false),
    (35, 3, 77, 96, false),
    (38, 10, 81, 110, false),
    (50, 4, 77, 90, false),
    (56, 4, 79, 100, false),
    (60, 4, 76, 85, true),
    (64, 3, 81, 105, false),
    (67, 3, 77, 95, false),
    (70, 10, 74, 100, false),
    (82, 4, 72, 85, false),
    (88, 3, 76, 95, false),
    (91, 5, 80, 108, false),
    (96, 14, 79, 110, true),
    (112, 4, 74, 85, false),
    (116, 4, 70, 80, false),
    (120, 6, 73, 95, false),
    (128, 3, 76, 104, false),
    (131, 3, 78, 100, false),
    (134, 10, 85, 118, false),
    (146, 3, 81, 96, false),
    (149, 3, 78, 92, false),
    (152, 6, 79, 108, false),
    (160, 3, 74, 104, false),
    (163, 3, 77, 100, false),
    (166, 10, 84, 115, false),
    (178, 4, 81, 94, false),
    (184, 4, 82, 104, false),
    (188, 4, 79, 88, true),
    (192, 3, 81, 110, false),
    (195, 3, 78, 100, false),
    (198, 8, 74, 104, false),
    (208, 4, 77, 100, false),
    (212, 4, 81, 106, false),
    (216, 4, 82, 112, false),
    (220, 4, 84, 118, false),
    (224, 18, 85, 124, true),
    (244, 10, 81, 100, true),
];
const BREAK_LEAD: &[(i64, i64, u8, u8, bool)] = &[
    (70, 12, 79, 82, false),
    (84, 8, 74, 72, false),
    (102, 14, 76, 80, false),
    (118, 8, 71, 70, false),
];

/// Level per bar (dB; −60 = silent) of chords, sub, lead and drums.
const CHORDS_DB: [f32; 80] = [
    -9.0, -8.0, -7.0, -6.0, -6.0, -6.0, -6.0, -5.0, -4.0, -4.0, -4.0, -4.0, -4.0, -4.0, -4.0, -4.0,
    -4.0, -4.0, -4.0, -4.0, -4.0, -4.0, -4.0, -4.0, -5.0, -4.5, -4.0, -3.5, -3.0, -2.5, -2.0, -1.0,
    -2.0, -2.0, -2.0, -2.0, -2.0, -2.0, -2.0, -2.0, -2.0, -2.0, -2.0, -2.0, -2.0, -2.0, -2.0, -2.0,
    -5.0, -5.0, -5.0, -5.0, -5.0, -5.0, -5.0, -5.0, -1.0, -1.0, -1.0, -1.0, -1.0, -1.0, -1.0, -1.0,
    -1.0, -1.0, -1.0, -1.0, -1.0, -1.0, -1.0, -1.0, -3.0, -3.0, -4.0, -5.0, -7.0, -9.0, -12.0,
    -16.0,
];
const SUB_DB: [f32; 80] = [
    -60.0, -60.0, -60.0, -60.0, -6.0, -6.0, -6.0, -6.0, -3.0, -3.0, -3.0, -3.0, -3.0, -3.0, -3.0,
    -3.0, -3.0, -3.0, -3.0, -3.0, -3.0, -3.0, -3.0, -3.0, -3.0, -3.0, -3.0, -3.0, -3.0, -3.0, -3.0,
    -60.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -8.0,
    -8.0, -8.0, -8.0, -8.0, -8.0, -8.0, -60.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
    0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -3.0, -3.0, -4.0, -5.0, -7.0, -9.0, -60.0, -60.0,
];
const LEAD_DB: [f32; 80] = [
    -60.0, -60.0, -60.0, -60.0, -60.0, -60.0, -60.0, -60.0, -60.0, -60.0, -60.0, -60.0, -60.0,
    -60.0, -60.0, -60.0, -60.0, -60.0, -60.0, -60.0, -60.0, -60.0, -60.0, -60.0, -60.0, -60.0,
    -60.0, -60.0, -60.0, -60.0, -60.0, -60.0, -1.0, -1.0, -1.0, -1.0, -1.0, -1.0, -1.0, -1.0, -1.0,
    -1.0, -1.0, -1.0, -1.0, -1.0, -1.0, -1.0, -60.0, -60.0, -60.0, -60.0, -8.0, -8.0, -7.0, -6.0,
    0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -60.0, -60.0,
    -60.0, -60.0, -60.0, -60.0, -60.0, -60.0,
];
const DRUMS_DB: [f32; 80] = [
    -60.0, -60.0, -60.0, -60.0, -9.0, -9.0, -8.0, -7.0, -3.0, -3.0, -3.0, -3.0, -3.0, -3.0, -3.0,
    -3.0, -2.0, -2.0, -2.0, -2.0, -2.0, -2.0, -2.0, -2.0, -6.0, -5.0, -4.0, -3.5, -3.0, -2.0, -1.0,
    0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -60.0,
    -60.0, -60.0, -60.0, -10.0, -10.0, -9.0, -6.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
    0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -4.0, -4.0, -5.0, -6.0, -8.0, -10.0, -60.0, -60.0,
];

// ---- arrangement ---------------------------------------------------------------------

/// How a part is struck from a bar on (until the next entry).
#[derive(Clone, Copy)]
enum Strikes {
    /// Silent.
    Off,
    /// Held through every chord.
    Held,
    /// Re-struck per bar: (step, length, velocity); the bars of the list take turns.
    Bars(&'static [&'static [(i64, i64, u8)]]),
}

/// Verse flow: long and short strikes.
const FLOW: &[&[(i64, i64, u8)]] = &[
    &[(0, 10, 112), (10, 6, 96)],
    &[(0, 6, 112), (6, 4, 92), (10, 6, 100)],
];
/// The drops: 1-1-2 sixteenths, then a 3+3+2 push, without gaps.
const DROP_GATE: &[&[(i64, i64, u8)]] = &[
    &[
        (0, 1, 118),
        (1, 1, 72),
        (2, 2, 96),
        (4, 1, 112),
        (5, 1, 72),
        (6, 2, 96),
        (8, 1, 118),
        (9, 1, 72),
        (10, 2, 96),
        (12, 1, 112),
        (13, 1, 72),
        (14, 2, 96),
    ],
    &[
        (0, 2, 118),
        (2, 1, 78),
        (3, 2, 108),
        (5, 1, 78),
        (6, 2, 96),
        (8, 2, 118),
        (10, 1, 78),
        (11, 2, 108),
        (13, 1, 78),
        (14, 2, 96),
    ],
];
const QUARTERS: &[&[(i64, i64, u8)]] = &[&[(0, 4, 90), (4, 4, 70), (8, 4, 80), (12, 4, 70)]];
const EIGHTHS: &[&[(i64, i64, u8)]] = &[&[
    (0, 2, 100),
    (2, 2, 70),
    (4, 2, 85),
    (6, 2, 70),
    (8, 2, 95),
    (10, 2, 70),
    (12, 2, 85),
    (14, 2, 75),
]];
const RISE_1: &[&[(i64, i64, u8)]] = &[&[
    (0, 1, 70),
    (1, 1, 72),
    (2, 1, 74),
    (3, 1, 76),
    (4, 1, 78),
    (5, 1, 80),
    (6, 1, 82),
    (7, 1, 84),
    (8, 1, 86),
    (9, 1, 88),
    (10, 1, 90),
    (11, 1, 92),
    (12, 1, 94),
    (13, 1, 96),
    (14, 1, 98),
    (15, 1, 100),
]];
const RISE_2: &[&[(i64, i64, u8)]] = &[&[
    (0, 1, 100),
    (1, 1, 102),
    (2, 1, 104),
    (3, 1, 105),
    (4, 1, 107),
    (5, 1, 109),
    (6, 1, 111),
    (7, 1, 113),
    (8, 1, 114),
    (9, 1, 116),
    (10, 1, 118),
    (11, 1, 120),
    (12, 1, 122),
    (13, 1, 123),
    (14, 1, 125),
    (15, 1, 127),
]];

/// Chords: (from bar, strikes, glide in s).
const CHORD_PLAN: &[(i64, Strikes, f32)] = &[
    (0, Strikes::Held, 0.3),
    (8, Strikes::Held, 0.2),
    (16, Strikes::Bars(FLOW), 0.15),
    (24, Strikes::Held, 0.1),
    (26, Strikes::Bars(QUARTERS), 0.05),
    (28, Strikes::Bars(EIGHTHS), 0.05),
    (30, Strikes::Bars(RISE_1), 0.05),
    (31, Strikes::Bars(RISE_2), 0.05),
    (32, Strikes::Bars(DROP_GATE), 0.06),
    (48, Strikes::Held, 0.4),
    (56, Strikes::Bars(DROP_GATE), 0.06),
    (72, Strikes::Held, 0.3),
    (80, Strikes::Off, 0.0),
];

const SUB_KICK: &[&[(i64, i64, u8)]] = &[&[(0, 8, 100), (10, 6, 100)]];
const SUB_QUARTERS: &[&[(i64, i64, u8)]] =
    &[&[(0, 3, 100), (4, 3, 100), (8, 3, 100), (12, 3, 100)]];
const SUB_EIGHTHS: &[&[(i64, i64, u8)]] = &[&[
    (0, 1, 100),
    (2, 1, 100),
    (4, 1, 100),
    (6, 1, 100),
    (8, 1, 100),
    (10, 1, 100),
    (12, 1, 100),
    (14, 1, 100),
]];

/// Sub: (from bar, strikes); it leaves the bar before each drop empty.
const SUB_PLAN: &[(i64, Strikes)] = &[
    (0, Strikes::Off),
    (4, Strikes::Held),
    (16, Strikes::Bars(SUB_KICK)),
    (24, Strikes::Held),
    (28, Strikes::Bars(SUB_QUARTERS)),
    (30, Strikes::Bars(SUB_EIGHTHS)),
    (31, Strikes::Off),
    (32, Strikes::Bars(SUB_KICK)),
    (48, Strikes::Held),
    (55, Strikes::Off),
    (56, Strikes::Bars(SUB_KICK)),
    (72, Strikes::Held),
    (78, Strikes::Off),
];

/// Drum hits: (key, step, velocity).
type Hits = &'static [(u8, i64, u8)];
const K: u8 = key::KICK;
const S: u8 = key::SNARE;
const C: u8 = key::CLAP;
const H: u8 = key::HAT;
const O: u8 = key::OPEN_HAT;

const HATS_SOFT: Hits = &[(H, 2, 60), (H, 6, 48), (H, 10, 60), (H, 14, 48)];
const PULSE: Hits = &[(K, 0, 90), (H, 2, 60), (H, 6, 48), (H, 10, 60), (H, 14, 48)];
const BEAT: Hits = &[
    (K, 0, 120),
    (K, 10, 105),
    (S, 8, 110),
    (H, 2, 85),
    (H, 6, 70),
    (H, 10, 85),
    (H, 14, 70),
];
const BEAT_OPEN: Hits = &[
    (K, 0, 120),
    (K, 10, 105),
    (S, 8, 110),
    (H, 2, 85),
    (H, 6, 70),
    (H, 10, 85),
    (O, 14, 72),
];
const BEAT_CLAP: Hits = &[
    (K, 0, 120),
    (K, 10, 105),
    (S, 8, 110),
    (C, 8, 80),
    (H, 2, 85),
    (H, 6, 70),
    (H, 10, 85),
    (H, 14, 70),
];
const BEAT_CLAP_OPEN: Hits = &[
    (K, 0, 120),
    (K, 10, 105),
    (S, 8, 110),
    (C, 8, 80),
    (H, 2, 85),
    (H, 6, 70),
    (H, 10, 85),
    (O, 14, 72),
];
const DROP: Hits = &[
    (K, 0, 127),
    (K, 10, 118),
    (S, 8, 122),
    (C, 8, 100),
    (H, 2, 95),
    (H, 6, 80),
    (H, 10, 95),
    (H, 14, 80),
];
const DROP_OPEN: Hits = &[
    (K, 0, 127),
    (K, 10, 118),
    (S, 8, 122),
    (C, 8, 100),
    (H, 2, 95),
    (H, 6, 80),
    (H, 10, 95),
    (O, 14, 85),
];
const DROP2_OPEN: Hits = &[
    (K, 0, 127),
    (K, 10, 118),
    (S, 8, 122),
    (C, 8, 105),
    (H, 2, 95),
    (H, 6, 80),
    (H, 10, 95),
    (O, 14, 85),
];
const DROP2_PUSH: Hits = &[
    (K, 0, 127),
    (K, 10, 118),
    (K, 14, 92),
    (S, 8, 122),
    (C, 8, 105),
    (H, 2, 95),
    (H, 6, 80),
    (H, 10, 95),
    (O, 14, 85),
    (H, 15, 55),
];
const BUILD_HALF: Hits = &[
    (K, 0, 100),
    (K, 8, 90),
    (H, 2, 65),
    (H, 6, 55),
    (H, 10, 65),
    (H, 14, 55),
];
const BUILD_FOUR: Hits = &[
    (K, 0, 105),
    (K, 4, 100),
    (K, 8, 105),
    (K, 12, 100),
    (H, 2, 75),
    (H, 6, 65),
    (H, 10, 75),
    (H, 14, 65),
];
const BUILD_5: Hits = &[
    (K, 0, 110),
    (K, 4, 105),
    (K, 8, 110),
    (K, 12, 105),
    (S, 0, 60),
    (S, 4, 65),
    (S, 8, 70),
    (S, 12, 75),
    (H, 2, 80),
    (H, 6, 70),
    (H, 10, 80),
    (H, 14, 70),
];
const BUILD_6: Hits = &[
    (K, 0, 110),
    (K, 4, 105),
    (K, 8, 110),
    (K, 12, 105),
    (S, 0, 80),
    (S, 4, 85),
    (S, 8, 90),
    (S, 12, 95),
    (H, 2, 80),
    (H, 6, 70),
    (H, 10, 80),
    (H, 14, 70),
];
const BUILD_7: Hits = &[
    (K, 0, 115),
    (K, 4, 110),
    (K, 8, 115),
    (K, 12, 110),
    (S, 0, 70),
    (S, 2, 75),
    (S, 4, 80),
    (S, 6, 85),
    (S, 8, 90),
    (S, 10, 95),
    (S, 12, 100),
    (S, 14, 105),
    (H, 2, 88),
    (H, 6, 78),
    (H, 10, 88),
    (H, 14, 78),
];
const BUILD_ROLL: Hits = &[
    (K, 0, 115),
    (S, 0, 60),
    (S, 1, 64),
    (S, 2, 69),
    (S, 3, 73),
    (S, 4, 78),
    (S, 5, 82),
    (S, 6, 87),
    (S, 7, 91),
    (S, 8, 96),
    (S, 9, 100),
    (S, 10, 105),
    (S, 11, 109),
    (S, 12, 114),
    (S, 13, 118),
    (S, 14, 123),
    (S, 15, 127),
];
const BREAK_PULSE: Hits = &[(K, 0, 85), (H, 2, 65), (H, 6, 55), (H, 10, 65), (H, 14, 55)];
const OUTRO_PULSE: Hits = PULSE;

/// Drums: (from bar, bars taking turns).
const DRUM_PLAN: &[(i64, &[Hits])] = &[
    (0, &[&[]]),
    (4, &[PULSE]),
    (8, &[BEAT, BEAT_OPEN]),
    (16, &[BEAT_CLAP, BEAT_CLAP_OPEN]),
    (24, &[BUILD_HALF]),
    (26, &[BUILD_FOUR]),
    (28, &[BUILD_5]),
    (29, &[BUILD_6]),
    (30, &[BUILD_7]),
    (31, &[BUILD_ROLL]),
    (32, &[DROP, DROP_OPEN]),
    (48, &[&[]]),
    (52, &[HATS_SOFT]),
    (54, &[BREAK_PULSE]),
    (56, &[DROP, DROP_OPEN]),
    (64, &[DROP2_OPEN, DROP2_PUSH]),
    (72, &[BEAT]),
    (76, &[OUTRO_PULSE]),
    (78, &[&[]]),
];

/// Fills: (bar, from step: the beat is muted from there, hits).
const FILLS: &[(i64, i64, Hits)] = &[
    (7, 12, &[(S, 12, 60), (S, 13, 70), (S, 14, 82), (S, 15, 96)]),
    (15, 12, &[(S, 12, 80), (S, 14, 95)]),
    (
        23,
        12,
        &[(S, 12, 85), (S, 13, 95), (S, 14, 105), (S, 15, 115)],
    ),
    (
        39,
        12,
        &[(S, 12, 80), (S, 13, 90), (S, 14, 105), (S, 15, 120)],
    ),
    (
        47,
        8,
        &[
            (S, 8, 122),
            (C, 8, 100),
            (S, 11, 90),
            (S, 12, 95),
            (S, 13, 100),
            (S, 14, 110),
            (S, 15, 120),
        ],
    ),
    (
        55,
        8,
        &[
            (S, 8, 60),
            (S, 9, 68),
            (S, 10, 77),
            (S, 11, 85),
            (S, 12, 94),
            (S, 13, 102),
            (S, 14, 111),
            (S, 15, 120),
        ],
    ),
    (
        63,
        12,
        &[(S, 12, 80), (S, 13, 90), (S, 14, 105), (S, 15, 120)],
    ),
    (
        71,
        12,
        &[
            (S, 12, 95),
            (S, 13, 105),
            (S, 14, 115),
            (S, 15, 125),
            (C, 15, 110),
        ],
    ),
];

/// Crash cymbals: (bar, velocity).
const CRASHES: &[(i64, u8)] = &[
    (8, 100),
    (16, 80),
    (24, 90),
    (32, 127),
    (40, 110),
    (48, 110),
    (56, 127),
    (64, 118),
    (72, 110),
];

/// Bars in the song.
const BARS: i64 = 80;

// ---- building ------------------------------------------------------------------------

/// Chords as (start, length, voicing) in sixteenths.
fn timeline() -> Vec<(i64, i64, &'static [u8])> {
    let mut at = 0;
    HARMONY
        .iter()
        .map(|&(v, len)| {
            at += len;
            (at - len, len, v)
        })
        .collect()
}

/// The strikes of a plan entry from `bar` to `end` (bars): (start, length, velocity).
fn strikes(s: Strikes, bar: i64, end: i64) -> Vec<(i64, i64, u8)> {
    match s {
        Strikes::Off => Vec::new(),
        Strikes::Held => vec![(bar * 16, (end - bar) * 16, 110)],
        Strikes::Bars(bars) => (bar..end)
            .flat_map(|b| {
                let pattern = bars
                    .get(((b - bar) as usize) % bars.len())
                    .copied()
                    .unwrap_or(&[]);
                pattern
                    .iter()
                    .map(move |&(st, len, v)| (b * 16 + st, len, v))
            })
            .collect(),
    }
}

/// Splits each strike where the chord changes: (start, length, velocity, voicing).
fn pieces(strikes: &[(i64, i64, u8)]) -> Vec<(i64, i64, u8, &'static [u8])> {
    let chords = timeline();
    strikes
        .iter()
        .flat_map(|&(s, len, v)| {
            chords.iter().filter_map(move |&(cs, cl, voicing)| {
                let (a, b) = (s.max(cs), (s + len).min(cs + cl));
                (a < b).then_some((a, b - a, v, voicing))
            })
        })
        .collect()
}

/// Notes end just before the next one: its voices still sound, so it glides out of them.
fn length(len: i64) -> i64 {
    len * STEP - STEP / 8
}

fn chord_notes() -> Vec<Note> {
    let mut out = Vec::new();
    for w in CHORD_PLAN.windows(2) {
        let ((bar, s, _), (end, _, _)) = (w[0], w[1]);
        for (at, len, v, voicing) in pieces(&strikes(s, bar, end)) {
            for &p in voicing {
                let vel = (u32::from(chord_velocity(p)) * u32::from(v) / 127).max(1) as u8;
                out.push(note(p, vel, at * STEP, length(len)));
            }
        }
    }
    out
}

fn sub_notes() -> Vec<Note> {
    let mut out = Vec::new();
    for (i, &(bar, s)) in SUB_PLAN.iter().enumerate() {
        let end = SUB_PLAN.get(i + 1).map_or(BARS, |n| n.0);
        for (at, len, v, voicing) in pieces(&strikes(s, bar, end)) {
            if let Some(&root) = voicing.first() {
                out.push(note(root - 12, v, at * STEP, length(len)));
            }
        }
    }
    out
}

fn drum_notes() -> Vec<Note> {
    let mut out = Vec::new();
    for bar in 0..BARS {
        let Some(&(from, bars)) = DRUM_PLAN.iter().rev().find(|(b, _)| *b <= bar) else {
            continue;
        };
        let beat = bars
            .get(((bar - from) as usize) % bars.len())
            .copied()
            .unwrap_or(&[]);
        let fill = FILLS.iter().find(|f| f.0 == bar);
        let muted_from = fill.map_or(16, |f| f.1);
        let hits = beat
            .iter()
            .filter(|h| h.1 < muted_from)
            .chain(fill.map_or(&[][..], |f| f.2));
        out.extend(hits.map(|&(k, st, v)| note(k, v, bar * BAR + st * STEP, STEP)));
        if let Some(&(_, v)) = CRASHES.iter().find(|c| c.0 == bar) {
            out.push(note(key::CRASH, v, bar * BAR, 4 * STEP));
        }
    }
    out
}

fn lead_notes() -> Vec<Note> {
    [(32, DROP_LEAD), (48, BREAK_LEAD), (56, DROP2_LEAD)]
        .iter()
        .flat_map(|&(bar, line)| {
            line.iter()
                .map(move |&(s, len, p, v, _)| note(p, v, bar * BAR + s * STEP, length(len)))
        })
        .collect()
}

/// The lead: Raw stab with one saw an octave below and some echo. Its just-intonation root
/// follows the chords by automation (it plays single notes, so it cannot find the root).
fn lead_part() -> Part {
    let mut params = preset(musiton_devices::POLYSYNTH, "Raw stab");
    params.extend([
        (ps::OSC2_LEVEL, 0.3),
        (ps::JUST_ROOT, 2.0),
        (ps::JUST_KEYS, 1.0),
        (ps::AMP_A, 0.005),
        (ps::AMP_R, 0.2),
        (ps::GLIDE_MODE, 0.0),
    ]);
    Part {
        name: "Lead",
        color: [240, 200, 90],
        trim_db: -3.0,
        instrument: musiton_devices::POLYSYNTH,
        params,
        effects: vec![
            ("delay", vec![(delay::id::MIX, 0.18)]),
            (
                "reverb",
                vec![(reverb::id::MIX, 0.3), (reverb::id::DECAY, 2.0)],
            ),
        ],
        notes: lead_notes(),
    }
}

/// Bars as automation positions.
fn at(step: i64) -> f64 {
    step as f64 / 16.0
}

/// Builds the song.
pub fn raw_song() -> Project {
    let mut project = assemble(Some("raw"), 120.0, BARS, 1, || {
        let mut sub = raw_sub_part(sub_notes(), 0.1);
        sub.trim_db -= 1.0;
        vec![
            raw_chord_part(chord_notes(), 0.3),
            sub,
            lead_part(),
            drums("Drums", "Punchy", drum_notes(), 6.0),
        ]
    });
    // Section levels, bar by bar.
    for (name, db) in [
        ("Raw chords", CHORDS_DB),
        ("Sub", SUB_DB),
        ("Lead", LEAD_DB),
        ("Drums", DRUMS_DB),
    ] {
        let points: Vec<(f64, f32)> = db.iter().enumerate().map(|(b, &v)| (b as f64, v)).collect();
        automate_volume(&mut project, name, &points);
    }
    // Chord glide per section.
    let glide: Vec<(f64, f32)> = CHORD_PLAN
        .windows(2)
        .flat_map(|w| [(w[0].0 as f64, w[0].2), (w[1].0 as f64 - 0.01, w[0].2)])
        .collect();
    automate(&mut project, "Raw chords", 0, ps::GLIDE, &glide);
    // The lead's root follows the chords, switching half a sixteenth before each change.
    let mut root: Vec<(f64, f32)> = Vec::new();
    for (s, _, voicing) in timeline() {
        let key = f32::from(voicing.first().copied().unwrap_or(0) % 12);
        let t = (at(s) - 1.0 / 32.0).max(0.0);
        if let Some(&(_, previous)) = root.last() {
            root.push((t - 0.001, previous));
        }
        root.push((t, key));
    }
    automate(&mut project, "Lead", 0, ps::JUST_KEY, &root);
    // Lead slides: glide only on the marked notes.
    let mut slide = vec![(0.0, 0.0)];
    for (bar, line) in [(32, DROP_LEAD), (48, BREAK_LEAD), (56, DROP2_LEAD)] {
        for &(s, _, _, _, on) in line {
            if on {
                let t = bar as f64 + at(s);
                slide.extend([
                    (t - 0.04, 0.0),
                    (t - 0.03, 0.12),
                    (t + 0.05, 0.12),
                    (t + 0.06, 0.0),
                ]);
            }
        }
    }
    automate(&mut project, "Lead", 0, ps::GLIDE, &slide);
    // Loud and dense like the reference (crest about 8 dB), without changing the sound.
    master_bus(&mut project, 12.0, false);
    project
}
