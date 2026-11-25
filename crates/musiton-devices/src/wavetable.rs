//! Wavetable synth: five built-in tables of eight frames each (computed from harmonic
//! spectra, band-limited per octave so high notes do not alias). The position knob,
//! the mod envelope and an LFO sweep through the frames; unison, sub oscillator,
//! resonant filter, 16 voices.

use std::sync::{Arc, OnceLock};

use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_dsp::env::Adsr;
use musiton_dsp::filter::{FilterMode, Svf};
use musiton_dsp::osc::sin_turns;
use musiton_engine::node::{AudioIo, AudioNode, Event, ProcessContext};

use crate::voices::{handle_event, note_hz, split_block, Params, Voice, Voices};

/// Number of voices.
const VOICES: usize = 16;
/// Output headroom.
const HEADROOM: f32 = 0.22;
/// Samples per frame.
const LEN: usize = 1024;
/// Frames per table.
const FRAMES: usize = 8;
/// Band-limited copies per frame: copy `m` holds harmonics up to `MAX_HARMONIC >> m`.
const MIPS: usize = 9;
const MAX_HARMONIC: usize = LEN / 2;
/// Most unison copies.
const MAX_UNISON: usize = 5;
/// Filter coefficients are recomputed every this many samples.
const FILTER_UPDATE: u32 = 8;

const TABLES: &[&str] = &[
    "Basic shapes",
    "Pulse width",
    "Vocal",
    "Harmonics",
    "Digital",
];

/// Parameter ids.
#[allow(missing_docs)]
pub mod id {
    pub const TABLE: u64 = 0;
    pub const POSITION: u64 = 1;
    pub const POS_ENV: u64 = 2;
    pub const POS_LFO: u64 = 3;
    pub const LFO_RATE: u64 = 4;
    pub const UNISON: u64 = 5;
    pub const DETUNE: u64 = 6;
    pub const WIDTH: u64 = 7;
    pub const SUB: u64 = 8;
    pub const CUTOFF: u64 = 9;
    pub const RESONANCE: u64 = 10;
    pub const FILTER_ENV: u64 = 11;
    pub const AMP_A: u64 = 12;
    pub const AMP_D: u64 = 13;
    pub const AMP_S: u64 = 14;
    pub const AMP_R: u64 = 15;
    pub const MOD_A: u64 = 16;
    pub const MOD_D: u64 = 17;
    pub const MOD_S: u64 = 18;
    pub const MOD_R: u64 = 19;
    pub const VOLUME: u64 = 20;
    pub const VELOCITY: u64 = 21;
}

/// All parameters (index = id).
pub const PARAMS: [ParamInfo; 22] = [
    ParamInfo::choice(id::TABLE, "Table", TABLES, 0),
    ParamInfo::new(id::POSITION, "Position", 0.0, 1.0, 0.0),
    ParamInfo::new(id::POS_ENV, "Env to position", -1.0, 1.0, 0.0),
    ParamInfo::new(id::POS_LFO, "LFO to position", 0.0, 1.0, 0.0),
    ParamInfo::new(id::LFO_RATE, "LFO rate", 0.05, 10.0, 0.5)
        .log()
        .unit("Hz"),
    ParamInfo::new(id::UNISON, "Unison", 1.0, MAX_UNISON as f32, 1.0).int(),
    ParamInfo::new(id::DETUNE, "Unison detune", 0.0, 50.0, 15.0).unit("ct"),
    ParamInfo::new(id::WIDTH, "Unison width", 0.0, 1.0, 0.6),
    ParamInfo::new(id::SUB, "Sub", 0.0, 1.0, 0.0),
    ParamInfo::new(id::CUTOFF, "Cutoff", 20.0, 20_000.0, 12_000.0)
        .log()
        .unit("Hz"),
    ParamInfo::new(id::RESONANCE, "Resonance", 0.0, 1.0, 0.1),
    ParamInfo::new(id::FILTER_ENV, "Filter env", -1.0, 1.0, 0.0),
    ParamInfo::new(id::AMP_A, "Attack", 0.001, 5.0, 0.005)
        .log()
        .unit("s"),
    ParamInfo::new(id::AMP_D, "Decay", 0.001, 5.0, 0.3)
        .log()
        .unit("s"),
    ParamInfo::new(id::AMP_S, "Sustain", 0.0, 1.0, 0.8),
    ParamInfo::new(id::AMP_R, "Release", 0.001, 10.0, 0.3)
        .log()
        .unit("s"),
    ParamInfo::new(id::MOD_A, "Mod attack", 0.001, 5.0, 0.01)
        .log()
        .unit("s"),
    ParamInfo::new(id::MOD_D, "Mod decay", 0.001, 5.0, 0.8)
        .log()
        .unit("s"),
    ParamInfo::new(id::MOD_S, "Mod sustain", 0.0, 1.0, 0.0),
    ParamInfo::new(id::MOD_R, "Mod release", 0.001, 10.0, 0.5)
        .log()
        .unit("s"),
    ParamInfo::new(id::VOLUME, "Volume", 0.0, 1.0, 0.7),
    ParamInfo::new(id::VELOCITY, "Velocity", 0.0, 1.0, 0.6),
];

/// All tables: `[table][frame][mip][sample]`, flattened.
#[derive(Debug)]
struct Tables {
    data: Vec<f32>,
}

impl Tables {
    fn offset(table: usize, frame: usize, mip: usize) -> usize {
        ((table * FRAMES + frame) * MIPS + mip) * LEN
    }

    /// Linear-interpolated sample at `phase` (turns) of one frame.
    #[inline]
    fn read(&self, at: usize, phase: f32) -> f32 {
        let x = phase * LEN as f32;
        let i = x as usize;
        let frac = x - i as f32;
        let a = self.data.get(at + (i & (LEN - 1))).copied().unwrap_or(0.0);
        let b = self
            .data
            .get(at + ((i + 1) & (LEN - 1)))
            .copied()
            .unwrap_or(0.0);
        a + (b - a) * frac
    }
}

/// Harmonic amplitude `h` (1-based) of frame position `x` (0 … 1) of `table`.
fn spectrum(table: usize, x: f32, h: usize) -> f32 {
    let hf = h as f32;
    let odd = h % 2 == 1;
    match table {
        0 => {
            // Sine → triangle → saw → square.
            let sine = if h == 1 { 1.0 } else { 0.0 };
            let tri = if odd {
                let sign = if (h / 2).is_multiple_of(2) { 1.0 } else { -1.0 };
                sign * 0.81 / (hf * hf)
            } else {
                0.0
            };
            let saw = 0.6 / hf;
            let square = if odd { 0.8 / hf } else { 0.0 };
            let shapes = [sine, tri, saw, square];
            let pos = x * 3.0;
            let i = (pos as usize).min(2);
            let f = pos - i as f32;
            let a = shapes.get(i).copied().unwrap_or(0.0);
            let b = shapes.get(i + 1).copied().unwrap_or(0.0);
            a + (b - a) * f
        }
        1 => {
            // Pulse from square (50 %) to a thin 5 % pulse.
            let w = 0.5 - 0.45 * x;
            (hf * std::f32::consts::PI * w).sin() / hf
        }
        2 => {
            // Vowels a → e → i → o → u as two formants (in harmonics of a 110 Hz note).
            const VOWELS: [(f32, f32); 5] = [
                (800.0, 1_150.0),
                (400.0, 1_700.0),
                (300.0, 2_300.0),
                (450.0, 800.0),
                (325.0, 700.0),
            ];
            let pos = x * 4.0;
            let i = (pos as usize).min(3);
            let f = pos - i as f32;
            let (a1, a2) = VOWELS.get(i).copied().unwrap_or((500.0, 1_500.0));
            let (b1, b2) = VOWELS.get(i + 1).copied().unwrap_or((500.0, 1_500.0));
            let f1 = (a1 + (b1 - a1) * f) / 110.0;
            let f2 = (a2 + (b2 - a2) * f) / 110.0;
            let peak = |c: f32, w: f32| (-((hf - c) / w).powi(2)).exp();
            (peak(f1, 1.5) + 0.7 * peak(f2, 2.5) + 0.15) / hf.powf(0.7)
        }
        3 => {
            // More and more harmonics, like pulling organ drawbars.
            let n = 1.0 + x * 40.0;
            if hf <= n {
                1.0 / hf.sqrt()
            } else {
                0.0
            }
        }
        _ => {
            // Sparse "digital" spectra that change from frame to frame.
            let frame = (x * 7.0).round() as usize;
            let hash = (h.wrapping_mul(2_654_435_761) ^ frame.wrapping_mul(40_503)) % 1_000;
            if hash > 550 || h == 1 {
                (hash as f32 / 1_000.0 + 0.3) / hf.powf(0.6)
            } else {
                0.0
            }
        }
    }
}

fn build_tables() -> Tables {
    let sine: Vec<f32> = (0..LEN)
        .map(|n| (std::f32::consts::TAU * n as f32 / LEN as f32).sin())
        .collect();
    let mut data = vec![0.0_f32; TABLES.len() * FRAMES * MIPS * LEN];
    for table in 0..TABLES.len() {
        for frame in 0..FRAMES {
            let x = frame as f32 / (FRAMES - 1) as f32;
            let amps: Vec<f32> = (1..=MAX_HARMONIC).map(|h| spectrum(table, x, h)).collect();
            let mut scale = 1.0;
            for mip in 0..MIPS {
                let top = MAX_HARMONIC >> mip;
                let mut wave = vec![0.0_f32; LEN];
                for (k, &a) in amps.iter().enumerate().take(top) {
                    if a.abs() < 1e-5 {
                        continue;
                    }
                    let h = k + 1;
                    for (n, w) in wave.iter_mut().enumerate() {
                        *w += a * sine[(h * n) & (LEN - 1)];
                    }
                }
                if mip == 0 {
                    // Every frame peaks at 1 (the band-limited copies use the same scale).
                    let peak = wave.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
                    scale = if peak > 0.0 { 1.0 / peak } else { 1.0 };
                }
                let at = Tables::offset(table, frame, mip);
                for (d, w) in data[at..at + LEN].iter_mut().zip(&wave) {
                    *d = w * scale;
                }
            }
        }
    }
    Tables { data }
}

/// The tables, computed once (off the audio thread, in `prepare`).
fn tables() -> Arc<Tables> {
    static TABLES_ONCE: OnceLock<Arc<Tables>> = OnceLock::new();
    TABLES_ONCE.get_or_init(|| Arc::new(build_tables())).clone()
}

#[derive(Debug, Clone)]
struct WtVoice {
    pitch: f32,
    vel: f32,
    phase: [f32; MAX_UNISON],
    sub: f32,
    amp: Adsr,
    modenv: Adsr,
    filter: [Svf; 2],
}

impl Default for WtVoice {
    fn default() -> Self {
        Self {
            pitch: 0.0,
            vel: 0.0,
            phase: std::array::from_fn(|i| (i as f32 * 0.618_034).fract()),
            sub: 0.0,
            amp: Adsr::default(),
            modenv: Adsr::default(),
            filter: Default::default(),
        }
    }
}

impl Voice for WtVoice {
    fn release(&mut self) {
        self.amp.gate_off();
        self.modenv.gate_off();
    }

    fn is_active(&self) -> bool {
        self.amp.is_active()
    }
}

/// The wavetable synth node (no inputs, stereo out).
#[derive(Debug)]
pub struct Wavetable {
    p: Params<22>,
    voices: Voices<WtVoice>,
    sample_rate: f32,
    tables: Option<Arc<Tables>>,
    lfo: f32,
    counter: u32,
}

impl Default for Wavetable {
    fn default() -> Self {
        Self {
            p: Params::new(&PARAMS),
            voices: Voices::default(),
            sample_rate: 48_000.0,
            tables: None,
            lfo: 0.0,
            counter: 0,
        }
    }
}

impl Wavetable {
    fn note_on(&mut self, key: u8, vel: f32) {
        let sens = self.p.target(id::VELOCITY);
        if let Some(v) = self.voices.note_on(key) {
            v.pitch = f32::from(key);
            v.vel = 1.0 - sens + sens * vel;
            v.amp.gate_on();
            v.modenv.gate_on();
        }
    }

    fn render(&mut self, io: &mut AudioIo, from: usize, to: usize) {
        let Some(tables) = self.tables.clone() else {
            return;
        };
        let sr = self.sample_rate;
        let p = &self.p;
        let amp = [id::AMP_A, id::AMP_D, id::AMP_S, id::AMP_R].map(|i| p.target(i));
        let modenv = [id::MOD_A, id::MOD_D, id::MOD_S, id::MOD_R].map(|i| p.target(i));
        for v in self.voices.all() {
            v.amp.set(amp[0], amp[1], amp[2], amp[3], sr);
            v.modenv.set(modenv[0], modenv[1], modenv[2], modenv[3], sr);
        }
        let table = self.p.index(id::TABLE).min(TABLES.len() - 1);
        let unison = self.p.index(id::UNISON).clamp(1, MAX_UNISON);
        let gain = self.voices.controllers.gain();
        let vibrato = self.voices.controllers.mod_wheel * 0.3;
        let norm = 1.0 / (unison as f32).sqrt();
        for i in from..to {
            self.p.tick();
            let p = &self.p;
            self.lfo = (self.lfo + p.get(id::LFO_RATE) / sr).fract();
            let lfo = sin_turns(self.lfo);
            let detune = p.get(id::DETUNE) / 100.0;
            let width = p.get(id::WIDTH);
            let sub = p.get(id::SUB);
            let base_pos = p.get(id::POSITION) + p.get(id::POS_LFO) * 0.5 * lfo;
            let pos_env = p.get(id::POS_ENV);
            let filt_env = p.get(id::FILTER_ENV) * 6.0;
            let cutoff = p.get(id::CUTOFF);
            let reso = p.get(id::RESONANCE);
            let update = self.counter.is_multiple_of(FILTER_UPDATE);
            self.counter = self.counter.wrapping_add(1);
            let (mut l, mut r) = (0.0, 0.0);
            for v in self.voices.active() {
                let a = v.amp.next_value() * v.vel;
                let m = v.modenv.next_value();
                let pos = (base_pos + pos_env * m).clamp(0.0, 1.0) * (FRAMES - 1) as f32;
                let f0 = (pos as usize).min(FRAMES - 2);
                let ff = pos - f0 as f32;
                let pitch = v.pitch + vibrato * lfo;
                let (mut sl, mut sr_) = (0.0, 0.0);
                for (k, ph) in v.phase.iter_mut().enumerate().take(unison) {
                    let spread = if unison > 1 {
                        2.0 * k as f32 / (unison - 1) as f32 - 1.0
                    } else {
                        0.0
                    };
                    let inc = note_hz(pitch + spread * detune) / sr;
                    // The band-limited copy whose top harmonic stays below Nyquist.
                    let max_h = 0.5 / inc.max(1e-6);
                    let mip = ((MAX_HARMONIC as f32 / max_h).log2().ceil().max(0.0) as usize)
                        .min(MIPS - 1);
                    let x0 = tables.read(Tables::offset(table, f0, mip), *ph);
                    let x1 = tables.read(Tables::offset(table, f0 + 1, mip), *ph);
                    let x = (x0 + (x1 - x0) * ff) * norm;
                    *ph = (*ph + inc).fract();
                    let pan = spread * width;
                    sl += x * (1.0 - pan.max(0.0));
                    sr_ += x * (1.0 + pan.min(0.0));
                }
                v.sub = (v.sub + note_hz(pitch - 12.0) / sr).fract();
                let s = sin_turns(v.sub) * sub;
                if update {
                    let c = cutoff * (m * filt_env).exp2();
                    for f in &mut v.filter {
                        f.set(c, reso, sr);
                    }
                }
                let [fl, fr] = &mut v.filter;
                l += fl.process(sl + s, FilterMode::LowPass) * a;
                r += fr.process(sr_ + s, FilterMode::LowPass) * a;
            }
            let k = HEADROOM * p.get(id::VOLUME) * gain;
            for (ch, y) in [l, r].into_iter().enumerate() {
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = y * k;
                }
            }
        }
    }
}

impl AudioNode for Wavetable {
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
        self.tables = Some(tables());
    }

    fn process(&mut self, _ctx: &ProcessContext, io: &mut AudioIo, events: &[Event]) {
        let n = io.frames();
        split_block(
            self,
            n,
            events,
            |s, a, b| s.render(io, a, b),
            |s, e| {
                if let Some((key, vel)) = handle_event(&mut s.voices, &mut s.p, e) {
                    s.note_on(key, vel);
                }
            },
        );
    }

    fn reset(&mut self) {
        for v in self.voices.all() {
            *v = WtVoice::default();
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
        "Sweep pad",
        &[
            (id::TABLE, 0.0),
            (id::POS_LFO, 0.8),
            (id::LFO_RATE, 0.15),
            (id::POSITION, 0.5),
            (id::UNISON, 4.0),
            (id::AMP_A, 0.8),
            (id::AMP_R, 1.5),
            (id::CUTOFF, 5_000.0),
        ],
    ),
    (
        "Talking lead",
        &[
            (id::TABLE, 2.0),
            (id::POS_ENV, 1.0),
            (id::MOD_D, 1.2),
            (id::UNISON, 2.0),
            (id::DETUNE, 8.0),
        ],
    ),
    (
        "Pulse bass",
        &[
            (id::TABLE, 1.0),
            (id::POSITION, 0.2),
            (id::POS_ENV, 0.6),
            (id::MOD_D, 0.25),
            (id::SUB, 0.6),
            (id::CUTOFF, 1_500.0),
            (id::FILTER_ENV, 0.4),
            (id::AMP_S, 1.0),
            (id::AMP_R, 0.1),
        ],
    ),
    (
        "Digital bells",
        &[
            (id::TABLE, 4.0),
            (id::POSITION, 0.6),
            (id::POS_ENV, -0.5),
            (id::AMP_D, 1.5),
            (id::AMP_S, 0.0),
            (id::AMP_R, 1.2),
        ],
    ),
    (
        "Organ swell",
        &[
            (id::TABLE, 3.0),
            (id::POS_ENV, 0.4),
            (id::MOD_A, 1.5),
            (id::MOD_S, 1.0),
            (id::POSITION, 0.1),
            (id::AMP_S, 1.0),
        ],
    ),
];
