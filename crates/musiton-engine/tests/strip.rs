//! Offline tests for channel strips, sends and meters.

use std::sync::Arc;

use musiton_core::ids::ParamId;
use musiton_core::{TempoMap, TimeSig};
use musiton_engine::node::{AudioIo, AudioNode, ProcessContext};
use musiton_engine::nodes::{
    pan_gains, Meter, SendGain, Strip, STRIP_AUDIBLE, STRIP_GAIN, STRIP_PAN,
};

const SR: f32 = 48_000.0;

/// Feeds `input` (stereo) through `node` in blocks; returns all output channels.
fn run(
    node: &mut dyn AudioNode,
    input: &[Vec<f32>],
    block: usize,
    before_block: &mut dyn FnMut(usize, &mut dyn AudioNode),
) -> Vec<Vec<f32>> {
    node.prepare(SR, block);
    let tempo = TempoMap::default();
    let frames = input[0].len();
    let mut out = vec![Vec::new(); node.num_outputs()];
    let mut done = 0;
    let mut k = 0;
    while done < frames {
        let n = block.min(frames - done);
        before_block(k, node);
        let ins: Vec<Vec<f32>> = input
            .iter()
            .map(|c| {
                let mut v = c[done..done + n].to_vec();
                v.resize(block, 0.0);
                v
            })
            .collect();
        let mut outs = vec![vec![0.0; block]; node.num_outputs()];
        let ctx = ProcessContext {
            sample_rate: SR,
            frames: n,
            playing: true,
            sample_pos: done as u64,
            tempo: &tempo,
            time_sig: TimeSig::default(),
            metronome: false,
            inputs: &[],
            recording: false,
            punch: None,
        };
        node.process(&ctx, &mut AudioIo::new(&ins, &mut outs, n), &[]);
        for (o, b) in out.iter_mut().zip(&outs) {
            o.extend_from_slice(&b[..n]);
        }
        done += n;
        k += 1;
    }
    out
}

fn sine(amp: f32, frames: usize) -> Vec<Vec<f32>> {
    let s: Vec<f32> = (0..frames)
        .map(|i| amp * (std::f32::consts::TAU * 1000.0 * i as f32 / SR).sin())
        .collect();
    vec![s.clone(), s]
}

#[test]
fn pan_law_is_minus_3_db_in_the_centre() {
    let (l, r) = pan_gains(0.0);
    assert!((l - 0.707_106_77).abs() < 1e-6 && (r - l).abs() < 1e-6);
    assert_eq!(pan_gains(-1.0), (1.0, 0.0));
    let (l, r) = pan_gains(1.0);
    assert!(l.abs() < 1e-6 && (r - 1.0).abs() < 1e-6);
    // Constant power everywhere.
    for p in [-0.7f32, -0.2, 0.3, 0.9] {
        let (l, r) = pan_gains(p);
        assert!((l * l + r * r - 1.0).abs() < 1e-5);
    }
}

#[test]
fn gain_and_pan_apply_and_meters_match_offline_levels() {
    let meter = Arc::new(Meter::default());
    let mut strip = Strip::new(0.5, 0.0, true, meter.clone());
    // 2 s so the 300 ms RMS window has settled.
    let input = sine(0.5, 96_000);
    let out = run(&mut strip, &input, 256, &mut |_, _| {});
    // 0.5 gain × −3 dB centre pan.
    let g = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
    for i in [100usize, 5_000, 95_999] {
        assert!((out[0][i] - input[0][i] * g).abs() < 1e-6);
        assert!((out[1][i] - input[1][i] * g).abs() < 1e-6);
        // Pre-fader taps carry the untouched input.
        assert_eq!(out[2][i], input[0][i]);
    }
    let peak = meter.take_peak();
    assert!((peak[0] - 0.5 * g).abs() < 1e-3, "peak {peak:?}");
    let rms = meter.rms();
    let expected = 0.5 * g / std::f32::consts::SQRT_2;
    assert!(
        (rms[0] - expected).abs() < 0.01 * expected,
        "rms {rms:?} vs {expected}"
    );
    // Taking the peak resets it.
    assert_eq!(meter.take_peak(), [0.0, 0.0]);
}

#[test]
fn fader_moves_are_smoothed_and_mute_fades() {
    let meter = Arc::new(Meter::default());
    let mut strip = Strip::new(1.0, -1.0, true, meter);
    let input = vec![vec![1.0; 48_000], vec![0.0; 48_000]];
    let out = run(&mut strip, &input, 128, &mut |k, n| {
        if k == 50 {
            n.set_param(STRIP_GAIN, 0.25);
        }
        if k == 200 {
            n.set_param(STRIP_AUDIBLE, 0.0);
        }
        if k == 300 {
            n.set_param(STRIP_PAN, 1.0);
        }
    });
    let l = &out[0];
    assert_eq!(l[50 * 128 - 1], 1.0);
    let worst = l
        .windows(2)
        .fold(0.0_f32, |m, w| m.max((w[1] - w[0]).abs()));
    assert!(worst < 0.002, "zipper/click: {worst}");
    assert!((l[150 * 128] - 0.25).abs() < 1e-6);
    assert_eq!(l[250 * 128], 0.0);
}

#[test]
fn send_scales_its_input() {
    let mut send = SendGain::new(0.5);
    let input = sine(0.8, 1_000);
    let out = run(&mut send, &input, 64, &mut |_, _| {});
    assert!((out[0][500] - input[0][500] * 0.5).abs() < 1e-6);
    assert_eq!(send.params().len(), 1);
    send.set_param(ParamId(0), 0.0);
}
