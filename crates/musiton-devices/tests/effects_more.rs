//! Behaviour and golden renders of the phase 18 effects (chorus, flanger, phaser, auto
//! filter, gate, transient shaper).

mod common;

use common::*;
use musiton_devices::create_effect;
use musiton_devices::effects::{autofilter, gate, phaser, transient};

fn render(type_id: &str, params: &[(u64, f32)], input: &[Vec<f32>; 2]) -> [Vec<f32>; 2] {
    let mut fx = create_effect(type_id, params).0;
    run(fx.as_mut(), input, 256, &[], &mut |_, _| {})
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

#[test]
fn golden_renders() {
    let input = test_signal();
    for name in [
        "chorus",
        "flanger",
        "phaser",
        "autofilter",
        "gate",
        "transient",
        "ducker",
    ] {
        let out = render(name, &[], &input);
        assert!(out.iter().flatten().all(|s| s.is_finite()), "{name}");
        assert_golden(name, &out);
    }
}

#[test]
fn modulation_effects_keep_the_level_and_widen_the_sound() {
    let input = sine(220.0, 0.4, 48_000);
    for name in ["chorus", "flanger", "phaser"] {
        let out = render(name, &[], &input);
        let ratio = rms(&out[0][4_800..]) / rms(&input[0][4_800..]);
        assert!((0.4..1.6).contains(&ratio), "{name}: level ratio {ratio}");
        let side: Vec<f32> = out[0].iter().zip(&out[1]).map(|(l, r)| l - r).collect();
        assert!(rms(&side[4_800..]) > 0.001, "{name}: no stereo movement");
    }
    // Mix 0 leaves the phaser transparent.
    let dry = render("phaser", &[(phaser::id::MIX, 0.0)], &input);
    let err = dry[0]
        .iter()
        .zip(&input[0])
        .fold(0.0_f32, |m, (a, b)| m.max((a - b).abs()));
    assert!(err < 1e-4, "{err}");
}

#[test]
fn auto_filter_with_a_low_cutoff_removes_highs() {
    let input = test_signal();
    let out = render(
        "autofilter",
        &[
            (autofilter::id::CUTOFF, 200.0),
            (autofilter::id::LFO_AMOUNT, 0.0),
            (autofilter::id::RESONANCE, 0.0),
        ],
        &input,
    );
    // The 3 kHz part of the test signal is mostly gone, the 100 Hz part stays.
    let hp = |x: &[f32]| -> Vec<f32> { x.windows(2).map(|w| w[1] - w[0]).collect() };
    assert!(rms(&hp(&out[0][..12_000])) < 0.2 * rms(&hp(&input[0][..12_000])));
    assert!(rms(&out[0][..12_000]) > 0.5 * rms(&input[0][..12_000]) * 0.6);
}

#[test]
fn gate_closes_on_the_quiet_tail_and_passes_loud_parts() {
    let input = test_signal();
    let out = render(
        "gate",
        &[(gate::id::THRESHOLD, -20.0), (gate::id::RELEASE, 0.02)],
        &input,
    );
    // Loud start passes, the quiet tail (−26 dB) is gated away.
    assert!(rms(&out[0][..2_400]) > 0.9 * rms(&input[0][..2_400]));
    assert!(rms(&out[0][21_000..24_000]) < 0.01 * rms(&input[0][21_000..24_000]));
}

#[test]
fn transient_shaper_attack_boosts_hits() {
    let input = test_signal();
    let plain = render("transient", &[], &input);
    let punchy = render("transient", &[(transient::id::ATTACK, 1.0)], &input);
    let soft = render("transient", &[(transient::id::ATTACK, -1.0)], &input);
    let start = |x: &[Vec<f32>; 2]| rms(&x[0][..480]);
    assert!(start(&punchy) > 1.3 * start(&plain));
    assert!(start(&soft) < 0.8 * start(&plain));
    // Later, steady parts stay about the same.
    let steady = |x: &[Vec<f32>; 2]| rms(&x[0][6_000..9_000]);
    assert!((steady(&punchy) / steady(&plain) - 1.0).abs() < 0.3);
}
