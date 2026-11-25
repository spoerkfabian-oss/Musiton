//! Delay-line modulation effects: chorus (several slowly moving voices) and flanger
//! (one short, swept delay with feedback). Both are stereo: the right side's LFO runs
//! ahead of the left by the "Spread" amount.

use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_dsp::delay::DelayLine;
use musiton_dsp::osc::sin_turns;
use musiton_engine::node::{AudioIo, AudioNode, Event, ProcessContext};

use crate::voices::Params;

/// Longest delay any of the two effects reads (seconds).
const MAX_DELAY: f32 = 0.05;

/// Chorus parameters.
pub mod chorus {
    use musiton_core::params::ParamInfo;

    /// Parameter ids.
    #[allow(missing_docs)]
    pub mod id {
        pub const RATE: u64 = 0;
        pub const DEPTH: u64 = 1;
        pub const DELAY: u64 = 2;
        pub const VOICES: u64 = 3;
        pub const SPREAD: u64 = 4;
        pub const MIX: u64 = 5;
    }

    /// Parameters.
    pub const PARAMS: [ParamInfo; 6] = [
        ParamInfo::new(id::RATE, "Rate", 0.05, 5.0, 0.8)
            .log()
            .unit("Hz"),
        ParamInfo::new(id::DEPTH, "Depth", 0.0, 1.0, 0.5),
        ParamInfo::new(id::DELAY, "Delay", 0.005, 0.03, 0.012).unit("s"),
        ParamInfo::new(id::VOICES, "Voices", 1.0, 4.0, 2.0).int(),
        ParamInfo::new(id::SPREAD, "Spread", 0.0, 1.0, 0.5),
        ParamInfo::new(id::MIX, "Mix", 0.0, 1.0, 0.5),
    ];
}

/// Flanger parameters.
pub mod flanger {
    use musiton_core::params::ParamInfo;

    /// Parameter ids.
    #[allow(missing_docs)]
    pub mod id {
        pub const RATE: u64 = 0;
        pub const DEPTH: u64 = 1;
        pub const DELAY: u64 = 2;
        pub const FEEDBACK: u64 = 3;
        pub const SPREAD: u64 = 4;
        pub const MIX: u64 = 5;
    }

    /// Parameters.
    pub const PARAMS: [ParamInfo; 6] = [
        ParamInfo::new(id::RATE, "Rate", 0.02, 5.0, 0.25)
            .log()
            .unit("Hz"),
        ParamInfo::new(id::DEPTH, "Depth", 0.0, 1.0, 0.7),
        ParamInfo::new(id::DELAY, "Delay", 0.0005, 0.01, 0.002)
            .log()
            .unit("s"),
        ParamInfo::new(id::FEEDBACK, "Feedback", -0.95, 0.95, 0.5),
        ParamInfo::new(id::SPREAD, "Spread", 0.0, 1.0, 0.25),
        ParamInfo::new(id::MIX, "Mix", 0.0, 1.0, 0.5),
    ];
}

/// Which of the two effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Chorus,
    Flanger,
}

/// The chorus or flanger effect.
#[derive(Debug)]
pub struct Modulation {
    kind: Kind,
    p: Params<6>,
    lines: [DelayLine; 2],
    /// The base delay glides slowly (like tape) so turning it never clicks.
    delay: f32,
    lfo: f32,
    sample_rate: f32,
}

impl Modulation {
    /// A chorus.
    pub fn chorus() -> Self {
        Self::new(Kind::Chorus, &chorus::PARAMS)
    }

    /// A flanger.
    pub fn flanger() -> Self {
        Self::new(Kind::Flanger, &flanger::PARAMS)
    }

    fn new(kind: Kind, params: &'static [ParamInfo; 6]) -> Self {
        Self {
            kind,
            p: Params::new(params),
            lines: Default::default(),
            delay: params[2].default,
            lfo: 0.0,
            sample_rate: 48_000.0,
        }
    }
}

impl AudioNode for Modulation {
    fn num_inputs(&self) -> usize {
        2
    }

    fn num_outputs(&self) -> usize {
        2
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        self.p.prepare(sample_rate);
        self.delay = self.p.target(2);
        let len = (MAX_DELAY * sample_rate) as usize + 4;
        self.lines = [DelayLine::new(len), DelayLine::new(len)];
    }

    fn process(&mut self, _ctx: &ProcessContext, io: &mut AudioIo, _events: &[Event]) {
        let sr = self.sample_rate;
        // Same ids for both: rate 0, depth 1, delay 2, mix 5, spread 4.
        let voices = if self.kind == Kind::Chorus {
            self.p.index(chorus::id::VOICES).clamp(1, 4)
        } else {
            1
        };
        let glide = 1.0 - (-1.0 / (0.25 * sr)).exp();
        for i in 0..io.frames() {
            self.p.tick();
            self.delay += (self.p.target(2) - self.delay) * glide;
            let p = &self.p;
            self.lfo = (self.lfo + p.get(0) / sr).fract();
            let depth = p.get(1);
            let base = self.delay;
            let spread = p.get(4) * 0.5;
            let mix = p.get(5);
            for ch in 0..2 {
                let x = io.input(ch).get(i).copied().unwrap_or(0.0);
                let phase = self.lfo + spread * ch as f32;
                let line = &mut self.lines[ch];
                let y = match self.kind {
                    Kind::Chorus => {
                        let mut sum = 0.0;
                        for v in 0..voices {
                            let m = sin_turns(phase + v as f32 / voices as f32);
                            // Delay swings ±40 % of its base time at full depth.
                            sum += line.read((base * (1.0 + 0.4 * depth * m)) * sr);
                        }
                        line.push(x);
                        sum / voices as f32
                    }
                    Kind::Flanger => {
                        // Sweeps from almost 0 up to the base delay (the classic jet).
                        let m = 0.5 + 0.5 * sin_turns(phase);
                        let d = (base * (1.0 - depth * m)).max(0.00005) * sr;
                        let wet = line.read(d.max(1.0));
                        let fb = p.get(flanger::id::FEEDBACK);
                        line.push(x + (wet * fb).clamp(-1.5, 1.5));
                        wet
                    }
                };
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = x * (1.0 - 0.5 * mix) + y * mix;
                }
            }
        }
    }

    fn reset(&mut self) {
        for l in &mut self.lines {
            l.clear();
        }
    }

    fn params(&self) -> &[ParamInfo] {
        match self.kind {
            Kind::Chorus => &chorus::PARAMS,
            Kind::Flanger => &flanger::PARAMS,
        }
    }

    fn set_param(&mut self, id: ParamId, value: f32) {
        self.p.set(id, value);
    }
}
