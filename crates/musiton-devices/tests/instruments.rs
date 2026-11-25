//! Behaviour every built-in synth shares (sound, pitch, level, release) and golden renders
//! of every factory preset.

mod common;

use common::*;
use musiton_devices::{create_instrument, InstrumentType, INSTRUMENTS};
use musiton_engine::node::{Event, EventKind};

/// Instruments that make their own sound (the sampler needs a sample).
fn synths() -> impl Iterator<Item = InstrumentType> {
    INSTRUMENTS.into_iter().filter(|t| !t.uses_samples)
}

fn is_drums(t: &InstrumentType) -> bool {
    t.type_id == "drums"
}

fn note(at: usize, key: u8, on: bool) -> (usize, Event) {
    let kind = if on {
        EventKind::NoteOn {
            ch: 0,
            key,
            vel: 0.8,
        }
    } else {
        EventKind::NoteOff { ch: 0, key }
    };
    (at, Event { offset: 0, kind })
}

fn play(
    t: &InstrumentType,
    preset: &[(u64, f32)],
    events: &[(usize, Event)],
    frames: usize,
) -> [Vec<f32>; 2] {
    let mut n = create_instrument(t.type_id, preset, &[]);
    let silence = [vec![0.0; frames], vec![0.0; frames]];
    run(n.as_mut(), &silence, 256, events, &mut |_, _| {})
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

fn magnitude(x: &[f32], hz: f64) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for (n, &s) in x.iter().enumerate() {
        let w = std::f64::consts::TAU * hz * n as f64 / f64::from(SR);
        let win = 0.5 - 0.5 * (std::f64::consts::TAU * n as f64 / x.len() as f64).cos();
        re += f64::from(s) * w.cos() * win;
        im -= f64::from(s) * w.sin() * win;
    }
    (re * re + im * im).sqrt()
}

/// A chord (or drum pattern) held for half a second, then released.
fn phrase(t: &InstrumentType) -> Vec<(usize, Event)> {
    let keys: &[u8] = if is_drums(t) {
        &[36, 38, 42, 46, 39, 45, 37, 56, 49]
    } else {
        &[48, 55, 60, 64]
    };
    let mut ev = Vec::new();
    for (i, &k) in keys.iter().enumerate() {
        let at = if is_drums(t) { i * 4_800 } else { i * 120 };
        ev.push(note(at, k, true));
        ev.push(note(24_000 + at, k, false));
    }
    ev
}

#[test]
fn every_preset_sounds_without_overload_and_dies_away() {
    for t in synths() {
        for (name, preset) in t.presets {
            let out = play(&t, preset, &phrase(&t), 4 * 48_000);
            let label = format!("{} / {name}", t.name);
            for ch in &out {
                assert!(ch.iter().all(|s| s.is_finite()), "{label}: not finite");
                let peak = ch.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
                assert!(peak < 1.0, "{label}: peak {peak}");
                assert!(rms(&ch[..48_000]) > 0.005, "{label}: too quiet");
            }
            let tail = rms(&out[0][3 * 48_000 + 24_000..]).max(rms(&out[1][3 * 48_000 + 24_000..]));
            assert!(
                tail < 0.003,
                "{label}: still sounds 3 s after release ({tail})"
            );
        }
    }
}

#[test]
fn notes_play_at_their_pitch() {
    for t in synths().filter(|t| !is_drums(t)) {
        let out = play(&t, &[], &[note(0, 69, true)], 48_000);
        let x = &out[0][9_600..38_400];
        let at = magnitude(x, 440.0);
        for off in [415.3, 466.2, 220.0, 880.0] {
            assert!(
                magnitude(x, off) < at,
                "{}: more energy at {off} Hz than at 440 Hz",
                t.name
            );
        }
    }
}

#[test]
fn golden_renders() {
    for t in synths() {
        for (name, preset) in t.presets.iter().take(2) {
            let out = play(&t, preset, &phrase(&t), 48_000);
            let file = format!("{}-{}", t.type_id, name.to_lowercase().replace(' ', "-"));
            assert_golden(&file, &out);
        }
    }
}

#[test]
fn wavetable_high_notes_do_not_alias() {
    use musiton_devices::wavetable::id;
    let t = musiton_devices::instrument_type("wavetable").unwrap();
    // The saw frame of the basic table; key 120 ≈ 8372 Hz. Aliases of a naive saw would
    // fall at ≈2.2 and 6.1 kHz.
    let out = play(
        t,
        &[(id::POSITION, 2.0 / 3.0), (id::CUTOFF, 20_000.0)],
        &[note(0, 120, true)],
        24_000,
    );
    let x = &out[0][4_800..14_400];
    let fund = magnitude(x, 8_372.0);
    for alias in [2_163.0, 6_118.0] {
        let db = 20.0 * (magnitude(x, alias) / fund).log10();
        assert!(db < -30.0, "alias at {alias} Hz: {db:.1} dB");
    }
}
