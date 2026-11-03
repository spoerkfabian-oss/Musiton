//! Channel strip (fader, pan, mute, meter) and send level.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use musiton_core::ids::ParamId;
use musiton_dsp::smooth::LinearRamp;

use crate::node::{AudioIo, AudioNode, Event, ParamInfo, ProcessContext};

/// Strip parameter: linear gain.
pub const STRIP_GAIN: ParamId = ParamId(0);
/// Strip parameter: pan −1…+1.
pub const STRIP_PAN: ParamId = ParamId(1);
/// Strip parameter: 1 = audible, 0 = muted (mute/solo result).
pub const STRIP_AUDIBLE: ParamId = ParamId(2);
/// Smoothing time for fader/pan/mute changes.
const SMOOTH_MS: f32 = 20.0;
/// RMS integration time.
const RMS_MS: f32 = 300.0;

const STRIP_PARAMS: [ParamInfo; 3] = [
    ParamInfo {
        id: STRIP_GAIN,
        name: "Volume",
        min: 0.0,
        max: 4.0,
        default: 1.0,
    },
    ParamInfo {
        id: STRIP_PAN,
        name: "Pan",
        min: -1.0,
        max: 1.0,
        default: 0.0,
    },
    ParamInfo {
        id: STRIP_AUDIBLE,
        name: "Audible",
        min: 0.0,
        max: 1.0,
        default: 1.0,
    },
];

/// Constant-power pan with −3 dB in the centre: (left gain, right gain).
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let theta = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    (theta.cos(), theta.sin())
}

/// Levels written by the audio thread, read by the UI (lock-free).
#[derive(Debug, Default)]
pub struct Meter {
    peak: [AtomicU32; 2],
    rms: [AtomicU32; 2],
}

impl Meter {
    /// Highest absolute sample per channel since the last call (resets it).
    pub fn take_peak(&self) -> [f32; 2] {
        [0, 1].map(|c| f32::from_bits(self.peak[c].swap(0, Ordering::Relaxed)))
    }

    /// Current RMS level per channel (≈300 ms window).
    pub fn rms(&self) -> [f32; 2] {
        [0, 1].map(|c| f32::from_bits(self.rms[c].load(Ordering::Relaxed)))
    }

    fn add_peak(&self, ch: usize, value: f32) {
        if let Some(a) = self.peak.get(ch) {
            // Single writer (audio thread): load + store is enough; the UI only swaps to 0.
            if value > f32::from_bits(a.load(Ordering::Relaxed)) {
                a.store(value.to_bits(), Ordering::Relaxed);
            }
        }
    }

    fn set_rms(&self, ch: usize, value: f32) {
        if let Some(a) = self.rms.get(ch) {
            a.store(value.to_bits(), Ordering::Relaxed);
        }
    }
}

/// Stereo channel strip. Inputs 0/1; outputs 0/1 = after fader and pan,
/// outputs 2/3 = before the fader (for pre-fader sends).
#[derive(Debug)]
pub struct Strip {
    gain: LinearRamp,
    pan_l: LinearRamp,
    pan_r: LinearRamp,
    audible: LinearRamp,
    init: (f32, f32, bool),
    meter: Arc<Meter>,
    mean_sq: [f32; 2],
    rms_coef: f32,
}

impl Strip {
    /// Strip with initial linear gain, pan and audible state; levels go to `meter`.
    pub fn new(gain: f32, pan: f32, audible: bool, meter: Arc<Meter>) -> Self {
        Self {
            gain: LinearRamp::new(gain, 1),
            pan_l: LinearRamp::new(1.0, 1),
            pan_r: LinearRamp::new(1.0, 1),
            audible: LinearRamp::new(1.0, 1),
            init: (gain, pan, audible),
            meter,
            mean_sq: [0.0; 2],
            rms_coef: 0.0,
        }
    }

    fn set_pan(&mut self, pan: f32) {
        let (l, r) = pan_gains(pan);
        self.pan_l.set_target(l);
        self.pan_r.set_target(r);
    }
}

impl AudioNode for Strip {
    fn num_inputs(&self) -> usize {
        2
    }

    fn num_outputs(&self) -> usize {
        4
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        let (gain, pan, audible) = self.init;
        let (l, r) = pan_gains(pan);
        let a = if audible { 1.0 } else { 0.0 };
        self.gain = LinearRamp::with_time(gain, SMOOTH_MS, sample_rate);
        self.pan_l = LinearRamp::with_time(l, SMOOTH_MS, sample_rate);
        self.pan_r = LinearRamp::with_time(r, SMOOTH_MS, sample_rate);
        self.audible = LinearRamp::with_time(a, SMOOTH_MS, sample_rate);
        self.rms_coef = 1.0 - (-1.0 / (RMS_MS * 0.001 * sample_rate.max(1.0))).exp();
    }

    fn process(&mut self, _ctx: &ProcessContext, io: &mut AudioIo, _events: &[Event]) {
        let n = io.frames();
        let mut peak = [0.0_f32; 2];
        for i in 0..n {
            let x = [
                io.input(0).get(i).copied().unwrap_or(0.0),
                io.input(1).get(i).copied().unwrap_or(0.0),
            ];
            let g = self.gain.next_value() * self.audible.next_value();
            let y = [
                x[0] * g * self.pan_l.next_value(),
                x[1] * g * self.pan_r.next_value(),
            ];
            for ch in 0..2 {
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = y[ch];
                }
                if let Some(o) = io.output(ch + 2).get_mut(i) {
                    *o = x[ch];
                }
                peak[ch] = peak[ch].max(y[ch].abs());
                self.mean_sq[ch] += (y[ch] * y[ch] - self.mean_sq[ch]) * self.rms_coef;
            }
        }
        for (ch, (p, ms)) in peak.iter().zip(self.mean_sq).enumerate() {
            self.meter.add_peak(ch, *p);
            self.meter.set_rms(ch, ms.max(0.0).sqrt());
        }
    }

    fn reset(&mut self) {
        self.mean_sq = [0.0; 2];
    }

    fn params(&self) -> &[ParamInfo] {
        &STRIP_PARAMS
    }

    fn set_param(&mut self, id: ParamId, value: f32) {
        match id {
            STRIP_GAIN => self.gain.set_target(value.max(0.0)),
            STRIP_PAN => self.set_pan(value),
            STRIP_AUDIBLE => self.audible.set_target(if value > 0.5 { 1.0 } else { 0.0 }),
            _ => {}
        }
    }
}

const SEND_PARAMS: [ParamInfo; 1] = [ParamInfo {
    id: ParamId(0),
    name: "Send level",
    min: 0.0,
    max: 4.0,
    default: 1.0,
}];

/// Stereo send level (param 0 = linear gain, smoothed).
#[derive(Debug)]
pub struct SendGain {
    gain: LinearRamp,
    init: f32,
}

impl SendGain {
    /// Send with initial linear gain.
    pub fn new(gain: f32) -> Self {
        Self {
            gain: LinearRamp::new(gain, 1),
            init: gain,
        }
    }
}

impl AudioNode for SendGain {
    fn num_inputs(&self) -> usize {
        2
    }

    fn num_outputs(&self) -> usize {
        2
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.gain = LinearRamp::with_time(self.init, SMOOTH_MS, sample_rate);
    }

    fn process(&mut self, _ctx: &ProcessContext, io: &mut AudioIo, _events: &[Event]) {
        for i in 0..io.frames() {
            let g = self.gain.next_value();
            for ch in 0..2 {
                let x = io.input(ch).get(i).copied().unwrap_or(0.0);
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = x * g;
                }
            }
        }
    }

    fn reset(&mut self) {}

    fn params(&self) -> &[ParamInfo] {
        &SEND_PARAMS
    }

    fn set_param(&mut self, id: ParamId, value: f32) {
        if id == ParamId(0) {
            self.gain.set_target(value.max(0.0));
        }
    }
}
