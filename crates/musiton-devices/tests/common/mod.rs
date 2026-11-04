//! Helpers: run nodes offline, golden files, test signals.
#![allow(dead_code, clippy::needless_range_loop)]

use std::path::PathBuf;

use musiton_core::{TempoMap, TimeSig};
use musiton_engine::node::{AudioIo, AudioNode, Event, ProcessContext};

pub const SR: f32 = 48_000.0;

/// Renders `input` (stereo) through `node` in blocks; `before` may change params per block.
pub fn run(
    node: &mut dyn AudioNode,
    input: &[Vec<f32>; 2],
    block: usize,
    events: &[(usize, Event)],
    before: &mut dyn FnMut(usize, &mut dyn AudioNode),
) -> [Vec<f32>; 2] {
    node.prepare(SR, block);
    let tempo = TempoMap::constant(120.0);
    let frames = input[0].len();
    let mut out = [Vec::with_capacity(frames), Vec::with_capacity(frames)];
    let mut done = 0;
    let mut k = 0;
    while done < frames {
        let n = block.min(frames - done);
        before(k, node);
        let mut ins: Vec<Vec<f32>> = input
            .iter()
            .map(|c| {
                let mut v = c[done..done + n].to_vec();
                v.resize(block, 0.0);
                v
            })
            .collect();
        ins.resize(node.num_inputs(), vec![0.0; block]);
        let mut outs = vec![vec![0.0; block]; node.num_outputs()];
        let ev: Vec<Event> = events
            .iter()
            .filter(|(at, _)| (done..done + n).contains(at))
            .map(|(at, e)| Event {
                offset: (at - done) as u32,
                kind: e.kind,
            })
            .collect();
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
            live_midi: &[],
        };
        node.process(&ctx, &mut AudioIo::new(&ins, &mut outs, n), &ev);
        for (o, b) in out.iter_mut().zip(&outs) {
            o.extend_from_slice(&b[..n]);
        }
        done += n;
        k += 1;
    }
    out
}

/// 0.5 s test signal: 100 Hz + 3 kHz with a decaying burst, an impulse, and a quiet tail.
pub fn test_signal() -> [Vec<f32>; 2] {
    let n = 24_000;
    let l: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            let env = if i < 12_000 { (-t * 6.0).exp() } else { 0.05 };
            let imp = if i == 15_000 { 0.9 } else { 0.0 };
            env * (0.6 * (std::f32::consts::TAU * 100.0 * t).sin()
                + 0.3 * (std::f32::consts::TAU * 3000.0 * t).sin())
                + imp
        })
        .collect();
    let r = l
        .iter()
        .enumerate()
        .map(|(i, x)| x * if i % 2 == 0 { 0.8 } else { 0.9 })
        .collect();
    [l, r]
}

/// A plain sine on both channels.
pub fn sine(hz: f32, amp: f32, frames: usize) -> [Vec<f32>; 2] {
    let s: Vec<f32> = (0..frames)
        .map(|i| amp * (std::f32::consts::TAU * hz * i as f32 / SR).sin())
        .collect();
    [s.clone(), s]
}

/// Compares stereo output with `tests/golden/<name>.wav` (MUSITON_BLESS=1 rewrites).
pub fn assert_golden(name: &str, out: &[Vec<f32>; 2]) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.wav"));
    if std::env::var_os("MUSITON_BLESS").is_some() {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: SR as u32,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..out[0].len() {
            w.write_sample(out[0][i]).unwrap();
            w.write_sample(out[1][i]).unwrap();
        }
        w.finalize().unwrap();
        return;
    }
    let mut r = hound::WavReader::open(&path)
        .unwrap_or_else(|e| panic!("missing golden {} ({e})", path.display()));
    let reference: Vec<f32> = r.samples::<f32>().map(Result::unwrap).collect();
    assert_eq!(reference.len(), out[0].len() * 2, "{name}: length differs");
    let mut worst = 0.0_f32;
    for i in 0..out[0].len() {
        worst = worst
            .max((reference[2 * i] - out[0][i]).abs())
            .max((reference[2 * i + 1] - out[1][i]).abs());
    }
    assert!(worst <= 1e-4, "{name}: differs from golden by {worst}");
}

/// Largest jump between neighbouring samples.
pub fn max_step(x: &[f32]) -> f32 {
    x.windows(2).fold(0.0, |m, w| m.max((w[1] - w[0]).abs()))
}

/// Peak absolute value.
pub fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |m, s| m.max(s.abs()))
}

/// RMS.
pub fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
}
