//! Effect behaviour and golden renders.

mod common;

use common::*;
use musiton_core::ids::ParamId;
use musiton_devices::create_effect;
use musiton_devices::effects::{compressor, delay, eq, limiter, reverb, saturation, utility};
use musiton_engine::node::AudioNode;

fn fx(type_id: &str, params: &[(u64, f32)]) -> Box<dyn AudioNode> {
    create_effect(type_id, params).0
}

fn render(type_id: &str, params: &[(u64, f32)], input: &[Vec<f32>; 2]) -> [Vec<f32>; 2] {
    run(
        fx(type_id, params).as_mut(),
        input,
        256,
        &[],
        &mut |_, _| {},
    )
}

/// Golden renders with non-default settings.
#[test]
fn golden_renders() {
    let cases: [(&str, &[(u64, f32)]); 7] = [
        (
            "utility",
            &[
                (utility::id::GAIN, -3.0),
                (utility::id::PAN, 0.3),
                (utility::id::WIDTH, 1.5),
                (utility::id::INVERT_R, 1.0),
            ],
        ),
        (
            "eq",
            &[
                (eq::PER_BAND * 2 + 2, 6.0),
                (eq::PER_BAND * 5 + 2, -9.0),
                (4, 1.0),
                (1, 80.0),
            ],
        ),
        (
            "compressor",
            &[
                (compressor::id::THRESHOLD, -24.0),
                (compressor::id::RATIO, 6.0),
                (compressor::id::MAKEUP, 6.0),
            ],
        ),
        (
            "delay",
            &[
                (delay::id::SYNC, 0.0),
                (delay::id::TIME, 0.05),
                (delay::id::FEEDBACK, 0.6),
                (delay::id::MIX, 0.5),
                (delay::id::PING_PONG, 1.0),
            ],
        ),
        (
            "reverb",
            &[
                (reverb::id::DECAY, 1.2),
                (reverb::id::MIX, 0.5),
                (reverb::id::SIZE, 0.7),
            ],
        ),
        (
            "saturation",
            &[(saturation::id::DRIVE, 18.0), (saturation::id::KIND, 2.0)],
        ),
        (
            "limiter",
            &[(limiter::id::INPUT, 12.0), (limiter::id::CEILING, -1.0)],
        ),
    ];
    let input = test_signal();
    for (name, params) in cases {
        let out = render(name, params, &input);
        assert!(
            out.iter().flatten().all(|s| s.is_finite()),
            "{name} not finite"
        );
        assert_golden(name, &out);
    }
}

#[test]
fn eq_band_gain_and_off_is_transparent() {
    let input = sine(1000.0, 0.5, 24_000);
    let flat = render("eq", &[], &input);
    assert!((rms(&flat[0][4_800..]) - rms(&input[0][4_800..])).abs() < 1e-3);
    // Band 4 (800 Hz bell) moved to 1 kHz with +6 dB.
    let boosted = render(
        "eq",
        &[(eq::PER_BAND * 3 + 1, 1000.0), (eq::PER_BAND * 3 + 2, 6.0)],
        &input,
    );
    let gain = 20.0 * (rms(&boosted[0][9_600..]) / rms(&input[0][9_600..])).log10();
    assert!((gain - 6.0).abs() < 0.3, "{gain} dB");
}

#[test]
fn compressor_reduces_loud_signals_by_its_ratio() {
    // Sine peaking at −6 dB, threshold −20 dB, ratio 4, no knee → 14 dB over → 3.5 dB out.
    let input = sine(200.0, 0.5, 48_000);
    let out = render(
        "compressor",
        &[
            (compressor::id::THRESHOLD, -20.0),
            (compressor::id::RATIO, 4.0),
            (compressor::id::KNEE, 0.0),
            (compressor::id::RELEASE, 1.0),
        ],
        &input,
    );
    let out_db = 20.0 * peak(&out[0][24_000..]).log10();
    assert!((out_db - (-20.0 + 14.0 / 4.0)).abs() < 1.0, "{out_db} dB");
    assert_eq!(compressor::gain_computer(-30.0, -20.0, 4.0, 0.0), 0.0);
}

#[test]
fn delay_echoes_after_its_time_and_syncs_to_tempo() {
    let mut imp = [vec![0.0; 48_000], vec![0.0; 48_000]];
    imp[0][0] = 1.0;
    imp[1][0] = 1.0;
    let out = render(
        "delay",
        &[
            (delay::id::SYNC, 0.0),
            (delay::id::TIME, 0.1),
            (delay::id::FEEDBACK, 0.0),
            (delay::id::MIX, 1.0),
            (delay::id::HIGH_CUT, 20_000.0),
        ],
        &imp,
    );
    let first = out[0].iter().position(|s| s.abs() > 0.1).unwrap();
    assert!((first as i32 - 4_800).abs() <= 2, "echo at {first}");
    // 1/8 at 120 BPM = 0.25 s.
    let synced = render(
        "delay",
        &[
            (delay::id::DIVISION, 3.0),
            (delay::id::FEEDBACK, 0.0),
            (delay::id::MIX, 1.0),
            (delay::id::HIGH_CUT, 20_000.0),
        ],
        &imp,
    );
    let first = synced[0].iter().position(|s| s.abs() > 0.1).unwrap();
    assert!((first as i32 - 12_000).abs() <= 2, "synced echo at {first}");
}

#[test]
fn reverb_tail_decays_with_its_decay_time() {
    let mut imp = [vec![0.0; 96_000], vec![0.0; 96_000]];
    imp[0][0] = 1.0;
    imp[1][0] = 1.0;
    let out = render(
        "reverb",
        &[
            (reverb::id::DECAY, 1.0),
            (reverb::id::MIX, 1.0),
            (reverb::id::DAMPING, 20_000.0),
            (reverb::id::PREDELAY, 0.0),
        ],
        &imp,
    );
    let early = rms(&out[0][4_800..9_600]);
    let later = rms(&out[0][52_800..57_600]);
    // Decay 1 s = −60 dB per second; 1 s apart → about 60 dB quieter (allow a wide margin).
    let drop = 20.0 * (early / later).log10();
    assert!((45.0..75.0).contains(&drop), "drop {drop} dB");
    assert!(rms(&out[1][4_800..9_600]) > 0.0);
}

#[test]
fn limiter_never_exceeds_the_ceiling_and_reports_latency() {
    let input = test_signal();
    let mut l = fx(
        "limiter",
        &[(limiter::id::INPUT, 18.0), (limiter::id::CEILING, -3.0)],
    );
    let out = run(l.as_mut(), &input, 256, &[], &mut |_, _| {});
    let ceil = 10f32.powf(-3.0 / 20.0);
    assert!(peak(&out[0]) <= ceil + 1e-6 && peak(&out[1]) <= ceil + 1e-6);
    assert_eq!(l.latency(), 72);
}

#[test]
fn saturation_adds_harmonics() {
    let input = sine(200.0, 0.5, 24_000);
    let clean = render(
        "saturation",
        &[(saturation::id::DRIVE, 0.0), (saturation::id::OUTPUT, 0.0)],
        &input,
    );
    let hot = render(
        "saturation",
        &[(saturation::id::DRIVE, 24.0), (saturation::id::KIND, 1.0)],
        &input,
    );
    // A clipped sine is squarer: its peak-to-RMS ratio drops towards 1.
    let crest = |x: &[f32]| peak(x) / rms(x);
    assert!(crest(&hot[0][4_800..]) < crest(&clean[0][4_800..]) * 0.8);
}

#[test]
fn utility_gain_and_phase() {
    let input = sine(300.0, 0.5, 4_800);
    let out = render(
        "utility",
        &[(utility::id::GAIN, -6.0206), (utility::id::INVERT_L, 1.0)],
        &input,
    );
    assert!((out[0][4_000] + input[0][4_000] * 0.5).abs() < 1e-3);
    assert!((out[1][4_000] - input[1][4_000] * 0.5).abs() < 1e-3);
    let mut u = fx("utility", &[]);
    u.set_param(ParamId(99), 1.0);
}
