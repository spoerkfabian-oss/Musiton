//! Offline tests for the phase-1 test tone.

use musiton_engine::rt::{no_alloc, AllocDisabler};
use musiton_engine::{render_offline, Engine, EngineShared, TEST_TONE_LEVEL};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

const SR: f32 = 48_000.0;

#[test]
fn silent_when_tone_off() {
    let shared = EngineShared::new();
    let mut engine = Engine::new(shared, SR, 256);
    let out = render_offline(&mut engine, 4_800, 2, 256);
    assert_eq!(out.len(), 9_600);
    assert!(out.iter().all(|&s| s == 0.0));
}

#[test]
fn tone_is_440hz_at_minus_12_db_on_all_channels() {
    let shared = EngineShared::new();
    shared.set_test_tone(true);
    let mut engine = Engine::new(shared, SR, 256);
    let out = render_offline(&mut engine, 48_000, 2, 256);
    let left: Vec<f32> = out.chunks(2).map(|f| f[0]).collect();
    assert!(out.chunks(2).all(|f| f[0] == f[1]));
    let peak = left[4_800..].iter().fold(0.0_f32, |m, s| m.max(s.abs()));
    assert!((peak - TEST_TONE_LEVEL).abs() < 1e-3, "peak {peak}");
    let rising = left
        .windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count();
    assert!((439..=441).contains(&rising), "rising crossings: {rising}");
}

#[test]
fn toggling_is_click_free() {
    let shared = EngineShared::new();
    let mut engine = Engine::new(shared.clone(), SR, 128);
    let mut out = Vec::new();
    for i in 0..40 {
        shared.set_test_tone(i % 10 < 5);
        out.extend(render_offline(&mut engine, 1_000, 1, 128));
    }
    // Largest step a full-level 440 Hz sine can make between two samples, plus margin.
    let max_step = TEST_TONE_LEVEL * (std::f32::consts::TAU * 440.0 / SR) * 1.1;
    let worst = out
        .windows(2)
        .fold(0.0_f32, |m, w| m.max((w[1] - w[0]).abs()));
    assert!(worst <= max_step, "step {worst} > {max_step}");
}

#[test]
fn odd_callback_sizes_match_block_rendering() {
    let make = || {
        let shared = EngineShared::new();
        shared.set_test_tone(true);
        Engine::new(shared, SR, 256)
    };
    let a = render_offline(&mut make(), 3_000, 2, 256);
    let b = render_offline(&mut make(), 3_000, 2, 1_000);
    assert_eq!(a, b);
}

#[test]
fn process_does_not_allocate() {
    let shared = EngineShared::new();
    shared.set_test_tone(true);
    let mut engine = Engine::new(shared, SR, 256);
    let mut buf = vec![0.0_f32; 512 * 2];
    no_alloc(|| {
        for _ in 0..100 {
            engine.process(&mut buf, 2);
        }
    });
}
