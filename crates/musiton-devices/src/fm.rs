//! FM synth: four sine operators with their own envelopes, eight algorithms (how the
//! operators modulate each other), feedback on operator 4, 16 voices.

use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_dsp::env::Adsr;
use musiton_dsp::osc::sin_turns;
use musiton_engine::node::{AudioIo, AudioNode, EventKind, ProcessContext};

use crate::voices::{handle_event, note_hz, split_block, Params, Voice, Voices};

/// Number of voices.
const VOICES: usize = 16;
/// Output headroom.
const HEADROOM: f32 = 0.2;
/// Phase deviation (in turns) of a modulator at full level.
const MOD_DEPTH: f32 = 1.6;

const ALGORITHMS: &[&str] = &[
    "4→3→2→1",
    "(3→2)+4 → 1",
    "2+3+4 → 1",
    "2→1, 4→3",
    "4 → 1+2+3",
    "4→3, 1, 2",
    "1+2+3+4",
    "3→2→1, 4",
];

/// Modulators of each operator (bit i = operator i+1) and the carriers, per algorithm.
const ROUTES: [([u8; 4], u8); 8] = [
    ([0b0010, 0b0100, 0b1000, 0], 0b0001),
    ([0b1010, 0b0100, 0, 0], 0b0001),
    ([0b1110, 0, 0, 0], 0b0001),
    ([0b0010, 0, 0b1000, 0], 0b0101),
    ([0b1000, 0b1000, 0b1000, 0], 0b0111),
    ([0, 0, 0b1000, 0], 0b0111),
    ([0, 0, 0, 0], 0b1111),
    ([0b0010, 0b0100, 0, 0], 0b1001),
];

/// Parameter ids. Operator `n` (0..4) uses `OP + n * OP_STRIDE + RATIO …`.
#[allow(missing_docs)]
pub mod id {
    pub const ALGORITHM: u64 = 0;
    pub const FEEDBACK: u64 = 1;
    pub const VOLUME: u64 = 2;
    pub const VELOCITY: u64 = 3;
    pub const OP: u64 = 4;
    pub const OP_STRIDE: u64 = 6;
    pub const RATIO: u64 = 0;
    pub const LEVEL: u64 = 1;
    pub const ATTACK: u64 = 2;
    pub const DECAY: u64 = 3;
    pub const SUSTAIN: u64 = 4;
    pub const RELEASE: u64 = 5;

    /// Id of parameter `p` (RATIO …) of operator `op` (0..4).
    pub const fn op(op: u64, p: u64) -> u64 {
        OP + op * OP_STRIDE + p
    }
}

const fn op_params(op: u64, names: [&'static str; 6], level: f32) -> [ParamInfo; 6] {
    [
        ParamInfo::new(id::op(op, id::RATIO), names[0], 0.5, 16.0, 1.0),
        ParamInfo::new(id::op(op, id::LEVEL), names[1], 0.0, 1.0, level),
        ParamInfo::new(id::op(op, id::ATTACK), names[2], 0.001, 5.0, 0.002)
            .log()
            .unit("s"),
        ParamInfo::new(id::op(op, id::DECAY), names[3], 0.01, 10.0, 1.0)
            .log()
            .unit("s"),
        ParamInfo::new(id::op(op, id::SUSTAIN), names[4], 0.0, 1.0, 0.5),
        ParamInfo::new(id::op(op, id::RELEASE), names[5], 0.005, 10.0, 0.3)
            .log()
            .unit("s"),
    ]
}

const OP1: [ParamInfo; 6] = op_params(
    0,
    [
        "Op1 ratio",
        "Op1 level",
        "Op1 attack",
        "Op1 decay",
        "Op1 sustain",
        "Op1 release",
    ],
    1.0,
);
const OP2: [ParamInfo; 6] = op_params(
    1,
    [
        "Op2 ratio",
        "Op2 level",
        "Op2 attack",
        "Op2 decay",
        "Op2 sustain",
        "Op2 release",
    ],
    0.1,
);
const OP3: [ParamInfo; 6] = op_params(
    2,
    [
        "Op3 ratio",
        "Op3 level",
        "Op3 attack",
        "Op3 decay",
        "Op3 sustain",
        "Op3 release",
    ],
    0.0,
);
const OP4: [ParamInfo; 6] = op_params(
    3,
    [
        "Op4 ratio",
        "Op4 level",
        "Op4 attack",
        "Op4 decay",
        "Op4 sustain",
        "Op4 release",
    ],
    0.0,
);

/// All parameters (index = id).
pub const PARAMS: [ParamInfo; 28] = [
    ParamInfo::choice(id::ALGORITHM, "Algorithm", ALGORITHMS, 0),
    ParamInfo::new(id::FEEDBACK, "Feedback", 0.0, 1.0, 0.0),
    ParamInfo::new(id::VOLUME, "Volume", 0.0, 1.0, 0.7),
    ParamInfo::new(id::VELOCITY, "Velocity", 0.0, 1.0, 0.7),
    OP1[0],
    OP1[1],
    OP1[2],
    OP1[3],
    OP1[4],
    OP1[5],
    OP2[0],
    OP2[1],
    OP2[2],
    OP2[3],
    OP2[4],
    OP2[5],
    OP3[0],
    OP3[1],
    OP3[2],
    OP3[3],
    OP3[4],
    OP3[5],
    OP4[0],
    OP4[1],
    OP4[2],
    OP4[3],
    OP4[4],
    OP4[5],
];

#[derive(Debug, Clone, Default)]
struct FmVoice {
    pitch: f32,
    vel: f32,
    phase: [f32; 4],
    env: [Adsr; 4],
    /// Last two outputs of operator 4 (feedback).
    fb: [f32; 2],
}

impl Voice for FmVoice {
    fn release(&mut self) {
        for e in &mut self.env {
            e.gate_off();
        }
    }

    fn is_active(&self) -> bool {
        // The voice sounds while a carrier envelope runs; modulators alone are silent.
        self.env.iter().any(Adsr::is_active)
    }
}

/// The FM synth node (no inputs, stereo out).
#[derive(Debug)]
pub struct Fm {
    p: Params<28>,
    voices: Voices<FmVoice>,
    sample_rate: f32,
    /// Vibrato (mod wheel) phase in turns.
    lfo: f32,
}

impl Default for Fm {
    fn default() -> Self {
        Self {
            p: Params::new(&PARAMS),
            voices: Voices::default(),
            sample_rate: 48_000.0,
            lfo: 0.0,
        }
    }
}

impl Fm {
    fn set_envelopes(&mut self) {
        let sr = self.sample_rate;
        let p = &self.p;
        let times: [[f32; 4]; 4] = std::array::from_fn(|op| {
            let o = op as u64;
            [
                p.target(id::op(o, id::ATTACK)),
                p.target(id::op(o, id::DECAY)),
                p.target(id::op(o, id::SUSTAIN)),
                p.target(id::op(o, id::RELEASE)),
            ]
        });
        for v in self.voices.all() {
            for (e, [a, d, s, r]) in v.env.iter_mut().zip(times) {
                e.set(a, d, s, r, sr);
            }
        }
    }

    fn note_on(&mut self, key: u8, vel: f32) {
        let sens = self.p.target(id::VELOCITY);
        if let Some(v) = self.voices.note_on(key) {
            v.pitch = f32::from(key);
            v.vel = 1.0 - sens + sens * vel;
            for e in &mut v.env {
                e.gate_on();
            }
        }
    }

    fn render(&mut self, io: &mut AudioIo, from: usize, to: usize) {
        self.set_envelopes();
        let sr = self.sample_rate;
        let (mods, carriers) = ROUTES
            .get(self.p.index(id::ALGORITHM))
            .copied()
            .unwrap_or(ROUTES[0]);
        let n_carriers = carriers.count_ones().max(1) as f32;
        let vibrato = self.voices.controllers.mod_wheel * 0.3;
        let gain = self.voices.controllers.gain();
        for i in from..to {
            self.p.tick();
            let p = &self.p;
            let ratio: [f32; 4] = std::array::from_fn(|o| p.get(id::op(o as u64, id::RATIO)));
            let level: [f32; 4] = std::array::from_fn(|o| p.get(id::op(o as u64, id::LEVEL)));
            let feedback = p.get(id::FEEDBACK);
            let vol = p.get(id::VOLUME) * gain * HEADROOM / n_carriers.sqrt();
            let vib = vibrato * sin_turns(self.lfo);
            self.lfo = (self.lfo + 5.5 / sr).fract();
            let mut out = 0.0;
            for v in self.voices.active() {
                let base = note_hz(v.pitch + vib) / sr;
                let mut y = [0.0_f32; 4];
                // Operators 4 → 1: every route goes from a higher to a lower operator.
                for op in (0..4).rev() {
                    let env = v.env.get_mut(op).map_or(0.0, Adsr::next_value);
                    let mut pm = 0.0;
                    for (m, ym) in y.iter().enumerate().skip(op + 1) {
                        if mods.get(op).is_some_and(|b| b & (1 << m) != 0) {
                            pm += ym;
                        }
                    }
                    if op == 3 {
                        pm += feedback * 0.5 * (v.fb[0] + v.fb[1]);
                    }
                    let phase = v.phase.get(op).copied().unwrap_or(0.0);
                    let s = sin_turns(phase + pm * MOD_DEPTH);
                    let carrier = carriers & (1 << op) != 0;
                    // Velocity makes modulators brighter and carriers louder.
                    let amt = level.get(op).copied().unwrap_or(0.0) * env * v.vel;
                    if let Some(out) = y.get_mut(op) {
                        *out = s * amt;
                    }
                    if let Some(ph) = v.phase.get_mut(op) {
                        *ph = (*ph + base * ratio.get(op).copied().unwrap_or(1.0)).fract();
                    }
                    if carrier {
                        out += s * amt;
                    }
                }
                v.fb = [y[3], v.fb[0]];
            }
            let s = out * vol;
            for ch in 0..2 {
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = s;
                }
            }
        }
    }
}

impl AudioNode for Fm {
    fn num_inputs(&self) -> usize {
        0
    }

    fn num_outputs(&self) -> usize {
        2
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        self.p.prepare(sample_rate);
        self.voices.prepare(VOICES);
    }

    fn process(
        &mut self,
        _ctx: &ProcessContext,
        io: &mut AudioIo,
        events: &[musiton_engine::node::Event],
    ) {
        let n = io.frames();
        split_block(
            self,
            n,
            events,
            |s, a, b| s.render(io, a, b),
            |s, e: &EventKind| {
                if let Some((key, vel)) = handle_event(&mut s.voices, &mut s.p, e) {
                    s.note_on(key, vel);
                }
            },
        );
    }

    fn reset(&mut self) {
        for v in self.voices.all() {
            *v = FmVoice::default();
        }
    }

    fn params(&self) -> &[ParamInfo] {
        &PARAMS
    }

    fn set_param(&mut self, id: ParamId, value: f32) {
        self.p.set(id, value);
    }
}

/// Factory presets.
pub const PRESETS: &[(&str, &[(u64, f32)])] = &[
    ("Init", &[]),
    (
        "E-piano",
        &[
            (id::ALGORITHM, 3.0),
            (id::op(0, id::DECAY), 3.0),
            (id::op(0, id::SUSTAIN), 0.0),
            (id::op(1, id::RATIO), 1.0),
            (id::op(1, id::LEVEL), 0.35),
            (id::op(1, id::DECAY), 1.2),
            (id::op(1, id::SUSTAIN), 0.0),
            (id::op(2, id::LEVEL), 0.5),
            (id::op(2, id::DECAY), 1.5),
            (id::op(2, id::SUSTAIN), 0.0),
            (id::op(3, id::RATIO), 14.0),
            (id::op(3, id::LEVEL), 0.15),
            (id::op(3, id::DECAY), 0.3),
            (id::op(3, id::SUSTAIN), 0.0),
        ],
    ),
    (
        "Bell",
        &[
            (id::ALGORITHM, 0.0),
            (id::op(0, id::DECAY), 4.0),
            (id::op(0, id::SUSTAIN), 0.0),
            (id::op(0, id::RELEASE), 2.0),
            (id::op(1, id::RATIO), 3.5),
            (id::op(1, id::LEVEL), 0.5),
            (id::op(1, id::DECAY), 3.0),
            (id::op(1, id::SUSTAIN), 0.0),
        ],
    ),
    (
        "Bass",
        &[
            (id::ALGORITHM, 1.0),
            (id::FEEDBACK, 0.3),
            (id::op(0, id::SUSTAIN), 0.8),
            (id::op(0, id::RELEASE), 0.08),
            (id::op(1, id::LEVEL), 0.6),
            (id::op(1, id::DECAY), 0.25),
            (id::op(1, id::SUSTAIN), 0.1),
            (id::op(3, id::LEVEL), 0.3),
            (id::op(3, id::DECAY), 0.15),
            (id::op(3, id::SUSTAIN), 0.0),
        ],
    ),
    (
        "Brass",
        &[
            (id::ALGORITHM, 2.0),
            (id::FEEDBACK, 0.5),
            (id::op(0, id::ATTACK), 0.04),
            (id::op(0, id::SUSTAIN), 0.9),
            (id::op(1, id::LEVEL), 0.45),
            (id::op(1, id::ATTACK), 0.08),
            (id::op(1, id::SUSTAIN), 0.7),
            (id::op(3, id::LEVEL), 0.25),
            (id::op(3, id::ATTACK), 0.1),
            (id::op(3, id::SUSTAIN), 0.6),
        ],
    ),
    (
        "Organ",
        &[
            (id::ALGORITHM, 6.0),
            (id::op(0, id::SUSTAIN), 1.0),
            (id::op(1, id::RATIO), 2.0),
            (id::op(1, id::LEVEL), 0.6),
            (id::op(1, id::SUSTAIN), 1.0),
            (id::op(2, id::RATIO), 3.0),
            (id::op(2, id::LEVEL), 0.4),
            (id::op(2, id::SUSTAIN), 1.0),
            (id::op(3, id::RATIO), 4.0),
            (id::op(3, id::LEVEL), 0.3),
            (id::op(3, id::SUSTAIN), 1.0),
        ],
    ),
];
