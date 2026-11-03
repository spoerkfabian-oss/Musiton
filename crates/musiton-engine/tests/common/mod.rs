//! Shared test helpers: golden files and simple test nodes.
#![allow(dead_code)]

use std::path::PathBuf;

use musiton_engine::node::{AudioIo, AudioNode, Event, ProcessContext};

/// Compares `samples` (mono) with `tests/golden/<name>.wav`.
/// Set `MUSITON_BLESS=1` to (re)write the reference file.
pub fn assert_golden(name: &str, samples: &[f32], sample_rate: u32) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.wav"));
    if std::env::var_os("MUSITON_BLESS").is_some() {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for &s in samples {
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
        return;
    }
    let mut r = hound::WavReader::open(&path).unwrap_or_else(|e| {
        panic!(
            "missing golden file {} ({e}); run with MUSITON_BLESS=1",
            path.display()
        )
    });
    assert_eq!(r.spec().sample_rate, sample_rate, "sample rate differs");
    let reference: Vec<f32> = r.samples::<f32>().map(Result::unwrap).collect();
    assert_eq!(reference.len(), samples.len(), "length differs");
    let (worst_i, worst) = samples
        .iter()
        .zip(&reference)
        .map(|(a, b)| (a - b).abs())
        .enumerate()
        .fold((0, 0.0_f32), |m, (i, d)| if d > m.1 { (i, d) } else { m });
    assert!(worst <= 1e-5, "{name}: sample {worst_i} differs by {worst}");
}

/// Outputs a constant value on all channels.
pub struct Const {
    pub value: f32,
    pub channels: usize,
}

impl AudioNode for Const {
    fn num_inputs(&self) -> usize {
        0
    }
    fn num_outputs(&self) -> usize {
        self.channels
    }
    fn prepare(&mut self, _: f32, _: usize) {}
    fn process(&mut self, _: &ProcessContext, io: &mut AudioIo, _: &[Event]) {
        for o in io.outputs() {
            o.fill(self.value);
        }
    }
    fn reset(&mut self) {}
}

/// Multiplies each input channel by `gain`; param 0 sets the gain.
pub struct Gain {
    pub gain: f32,
    pub channels: usize,
}

impl AudioNode for Gain {
    fn num_inputs(&self) -> usize {
        self.channels
    }
    fn num_outputs(&self) -> usize {
        self.channels
    }
    fn prepare(&mut self, _: f32, _: usize) {}
    fn process(&mut self, _: &ProcessContext, io: &mut AudioIo, _: &[Event]) {
        for ch in 0..self.channels {
            let n = io.frames();
            for i in 0..n {
                let x = io.input(ch).get(i).copied().unwrap_or(0.0);
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = x * self.gain;
                }
            }
        }
    }
    fn reset(&mut self) {}
    fn set_param(&mut self, _: musiton_core::ids::ParamId, v: f32) {
        self.gain = v;
    }
}

/// Click onsets: the sample before the first non-silent sample after ≥ 100 silent ones.
/// (The metronome's sine starts at phase 0, so its first sample is exactly 0.)
pub fn onsets(samples: &[f32]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut quiet = usize::MAX / 2;
    for (i, s) in samples.iter().enumerate() {
        if s.abs() > 1e-6 {
            if quiet >= 100 {
                out.push(i.saturating_sub(1));
            }
            quiet = 0;
        } else {
            quiet += 1;
        }
    }
    out
}
