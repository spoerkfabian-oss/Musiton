//! Drum synth: every sound is synthesized (no samples). Keys follow the General MIDI drum
//! map: 36 kick, 37 rim, 38/40 snare, 39 clap, 41/43 low tom, 45/47 mid tom, 48/50 high
//! tom, 42/44 closed hi-hat, 46 open hi-hat (cut by the closed one), 49/57 crash, 56 cowbell.

use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_dsp::filter::{FilterMode, Svf};
use musiton_dsp::osc::{sin_turns, BlepOsc, Noise, Wave};
use musiton_engine::node::{AudioIo, AudioNode, Event, EventKind, ProcessContext};

use crate::controllers::Controllers;
use crate::voices::{split_block, Params};

/// Output headroom.
const HEADROOM: f32 = 0.5;
/// Square-wave partials of the metallic hi-hat and cymbal sound (Hz).
const METAL: [f32; 6] = [205.3, 304.4, 369.6, 522.7, 540.0, 800.0];

/// Parameter ids.
#[allow(missing_docs)]
pub mod id {
    pub const KICK_TUNE: u64 = 0;
    pub const KICK_DECAY: u64 = 1;
    pub const KICK_PUNCH: u64 = 2;
    pub const KICK_CLICK: u64 = 3;
    pub const KICK_LEVEL: u64 = 4;
    pub const SNARE_TUNE: u64 = 5;
    pub const SNARE_TONE: u64 = 6;
    pub const SNARE_DECAY: u64 = 7;
    pub const SNARE_LEVEL: u64 = 8;
    pub const CLAP_DECAY: u64 = 9;
    pub const CLAP_LEVEL: u64 = 10;
    pub const HAT_TONE: u64 = 11;
    pub const HAT_DECAY: u64 = 12;
    pub const OPEN_DECAY: u64 = 13;
    pub const HAT_LEVEL: u64 = 14;
    pub const TOM_TUNE: u64 = 15;
    pub const TOM_DECAY: u64 = 16;
    pub const TOM_LEVEL: u64 = 17;
    pub const PERC_LEVEL: u64 = 18;
    pub const CYMBAL_DECAY: u64 = 19;
    pub const CYMBAL_LEVEL: u64 = 20;
    pub const VOLUME: u64 = 21;
}

/// All parameters (index = id).
pub const PARAMS: [ParamInfo; 22] = [
    ParamInfo::new(id::KICK_TUNE, "Kick tune", 30.0, 120.0, 52.0).unit("Hz"),
    ParamInfo::new(id::KICK_DECAY, "Kick decay", 0.05, 2.0, 0.45)
        .log()
        .unit("s"),
    ParamInfo::new(id::KICK_PUNCH, "Kick punch", 0.0, 1.0, 0.6),
    ParamInfo::new(id::KICK_CLICK, "Kick click", 0.0, 1.0, 0.3),
    ParamInfo::new(id::KICK_LEVEL, "Kick level", 0.0, 1.0, 0.9),
    ParamInfo::new(id::SNARE_TUNE, "Snare tune", 100.0, 400.0, 185.0).unit("Hz"),
    ParamInfo::new(id::SNARE_TONE, "Snare tone", 0.0, 1.0, 0.4),
    ParamInfo::new(id::SNARE_DECAY, "Snare decay", 0.05, 1.0, 0.22)
        .log()
        .unit("s"),
    ParamInfo::new(id::SNARE_LEVEL, "Snare level", 0.0, 1.0, 0.8),
    ParamInfo::new(id::CLAP_DECAY, "Clap decay", 0.05, 1.0, 0.25)
        .log()
        .unit("s"),
    ParamInfo::new(id::CLAP_LEVEL, "Clap level", 0.0, 1.0, 0.7),
    ParamInfo::new(id::HAT_TONE, "Hat tone", 3_000.0, 14_000.0, 8_000.0)
        .log()
        .unit("Hz"),
    ParamInfo::new(id::HAT_DECAY, "Closed hat decay", 0.01, 0.3, 0.06)
        .log()
        .unit("s"),
    ParamInfo::new(id::OPEN_DECAY, "Open hat decay", 0.1, 2.0, 0.5)
        .log()
        .unit("s"),
    ParamInfo::new(id::HAT_LEVEL, "Hat level", 0.0, 1.0, 0.5),
    ParamInfo::new(id::TOM_TUNE, "Tom tune", 60.0, 250.0, 110.0).unit("Hz"),
    ParamInfo::new(id::TOM_DECAY, "Tom decay", 0.1, 2.0, 0.5)
        .log()
        .unit("s"),
    ParamInfo::new(id::TOM_LEVEL, "Tom level", 0.0, 1.0, 0.7),
    ParamInfo::new(id::PERC_LEVEL, "Rim/cowbell level", 0.0, 1.0, 0.5),
    ParamInfo::new(id::CYMBAL_DECAY, "Crash decay", 0.3, 4.0, 1.5)
        .log()
        .unit("s"),
    ParamInfo::new(id::CYMBAL_LEVEL, "Crash level", 0.0, 1.0, 0.4),
    ParamInfo::new(id::VOLUME, "Volume", 0.0, 1.0, 0.8),
];

/// Which sound a key plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Piece {
    Kick,
    Rim,
    Snare,
    Clap,
    Tom(usize),
    ClosedHat,
    OpenHat,
    Crash,
    Cowbell,
}

fn piece(key: u8) -> Option<Piece> {
    Some(match key {
        35 | 36 => Piece::Kick,
        37 => Piece::Rim,
        38 | 40 => Piece::Snare,
        39 => Piece::Clap,
        41 | 43 => Piece::Tom(0),
        45 | 47 => Piece::Tom(1),
        48 | 50 => Piece::Tom(2),
        42 | 44 => Piece::ClosedHat,
        46 => Piece::OpenHat,
        49 | 57 => Piece::Crash,
        56 => Piece::Cowbell,
        _ => return None,
    })
}

/// A decaying hit: amplitude envelope with a short fade-in (no click when retriggered).
#[derive(Debug, Clone, Copy, Default)]
struct Hit {
    amp: f32,
    level: f32,
    vel: f32,
    /// Samples since the hit.
    age: u32,
}

impl Hit {
    fn trigger(&mut self, vel: f32) {
        self.amp = 1.0;
        self.vel = vel;
        self.age = 0;
    }

    /// Next envelope value with decay coefficient `decay`.
    #[inline]
    fn next(&mut self, decay: f32) -> f32 {
        self.amp *= decay;
        self.level += (self.amp - self.level) * 0.35;
        self.age = self.age.saturating_add(1);
        self.level * self.vel
    }

    fn active(&self) -> bool {
        self.amp > 1e-5 || self.level > 1e-5
    }
}

/// The drum synth node (no inputs, stereo out).
#[derive(Debug)]
pub struct Drums {
    p: Params<22>,
    controllers: Controllers,
    sample_rate: f32,
    noise: Noise,
    kick: Hit,
    kick_phase: f32,
    rim: Hit,
    snare: Hit,
    snare_phase: [f32; 2],
    snare_hp: Svf,
    clap: Hit,
    clap_bp: Svf,
    toms: [Hit; 3],
    tom_phase: [f32; 3],
    hat: Hit,
    open: bool,
    metal: [BlepOsc; 6],
    hat_bp: Svf,
    hat_hp: Svf,
    crash: Hit,
    crash_hp: Svf,
    cowbell: Hit,
    bell_osc: [BlepOsc; 2],
    bell_bp: Svf,
}

impl Default for Drums {
    fn default() -> Self {
        Self {
            p: Params::new(&PARAMS),
            controllers: Controllers::default(),
            sample_rate: 48_000.0,
            noise: Noise::default(),
            kick: Hit::default(),
            kick_phase: 0.0,
            rim: Hit::default(),
            snare: Hit::default(),
            snare_phase: [0.0; 2],
            snare_hp: Svf::default(),
            clap: Hit::default(),
            clap_bp: Svf::default(),
            toms: [Hit::default(); 3],
            tom_phase: [0.0; 3],
            hat: Hit::default(),
            open: false,
            metal: Default::default(),
            hat_bp: Svf::default(),
            hat_hp: Svf::default(),
            crash: Hit::default(),
            crash_hp: Svf::default(),
            cowbell: Hit::default(),
            bell_osc: Default::default(),
            bell_bp: Svf::default(),
        }
    }
}

impl Drums {
    fn note_on(&mut self, key: u8, vel: f32) {
        let vel = 0.3 + 0.7 * vel;
        match piece(key) {
            Some(Piece::Kick) => self.kick.trigger(vel),
            Some(Piece::Rim) => self.rim.trigger(vel),
            Some(Piece::Snare) => self.snare.trigger(vel),
            Some(Piece::Clap) => self.clap.trigger(vel),
            Some(Piece::Tom(t)) => {
                if let Some(h) = self.toms.get_mut(t) {
                    h.trigger(vel);
                }
            }
            Some(Piece::ClosedHat) => {
                self.hat.trigger(vel);
                self.open = false;
            }
            Some(Piece::OpenHat) => {
                self.hat.trigger(vel);
                self.open = true;
            }
            Some(Piece::Crash) => self.crash.trigger(vel),
            Some(Piece::Cowbell) => self.cowbell.trigger(vel),
            None => {}
        }
    }

    fn coef(&self, secs: f32) -> f32 {
        (-6.9 / (secs.max(0.001) * self.sample_rate)).exp()
    }

    fn render(&mut self, io: &mut AudioIo, from: usize, to: usize) {
        let sr = self.sample_rate;
        let gain = self.controllers.gain();
        for i in from..to {
            self.p.tick();
            let p = &self.p;
            let noise = self.noise.next_sample();
            let (mut l, mut r) = (0.0, 0.0);
            let mut add = |x: f32, pan: f32| {
                l += x * (1.0 - pan.max(0.0));
                r += x * (1.0 + pan.min(0.0));
            };

            if self.kick.active() {
                let age = self.kick.age as f32 / sr;
                let env = self.kick.next((-6.9 / (p.get(id::KICK_DECAY) * sr)).exp());
                // Pitch falls from up to 7× the tune to the tune within ~40 ms.
                let sweep = 1.0 + 6.0 * p.get(id::KICK_PUNCH) * (-age / 0.012).exp();
                self.kick_phase = (self.kick_phase + p.get(id::KICK_TUNE) * sweep / sr).fract();
                let click = p.get(id::KICK_CLICK) * noise * (-age / 0.002).exp();
                let body = sin_turns(self.kick_phase);
                add(
                    (body * 1.4 + click).tanh() * env * p.get(id::KICK_LEVEL),
                    0.0,
                );
            }

            if self.snare.active() {
                let age = self.snare.age as f32 / sr;
                let decay = p.get(id::SNARE_DECAY);
                let env = self.snare.next((-6.9 / (decay * sr)).exp());
                let tune = p.get(id::SNARE_TUNE) * (1.0 + 0.5 * (-age / 0.01).exp());
                for (ph, ratio) in self.snare_phase.iter_mut().zip([1.0, 1.6]) {
                    *ph = (*ph + tune * ratio / sr).fract();
                }
                let body = (sin_turns(self.snare_phase[0]) + 0.6 * sin_turns(self.snare_phase[1]))
                    * (-age / (decay * 0.35)).exp();
                if i % 8 == 0 {
                    self.snare_hp.set(1_800.0, 0.1, sr);
                }
                let rattle = self.snare_hp.process(noise, FilterMode::HighPass);
                let tone = p.get(id::SNARE_TONE);
                add(
                    (body * tone + rattle * (1.0 - 0.5 * tone)) * env * p.get(id::SNARE_LEVEL),
                    -0.1,
                );
            }

            if self.clap.active() {
                let age = self.clap.age as f32 / sr;
                let env = self.clap.next((-6.9 / (p.get(id::CLAP_DECAY) * sr)).exp());
                // Three quick claps (10 ms apart) before the tail.
                let burst = if age < 0.03 {
                    let t = (age / 0.01).fract();
                    (-t * 8.0).exp() * 2.0
                } else {
                    1.0
                };
                if i % 8 == 0 {
                    self.clap_bp.set(1_200.0, 0.4, sr);
                }
                let x = self.clap_bp.process(noise, FilterMode::BandPass);
                add(x * burst * env * p.get(id::CLAP_LEVEL) * 2.0, 0.15);
            }

            for (t, (hit, ph)) in self.toms.iter_mut().zip(&mut self.tom_phase).enumerate() {
                if !hit.active() {
                    continue;
                }
                let age = hit.age as f32 / sr;
                let env = hit.next((-6.9 / (p.get(id::TOM_DECAY) * sr)).exp());
                let ratio = [1.0, 1.33, 1.78].get(t).copied().unwrap_or(1.0);
                let hz = p.get(id::TOM_TUNE) * ratio * (1.0 + 0.3 * (-age / 0.03).exp());
                *ph = (*ph + hz / sr).fract();
                let pan = [-0.4, 0.0, 0.4].get(t).copied().unwrap_or(0.0);
                add(sin_turns(*ph) * env * p.get(id::TOM_LEVEL), pan);
            }

            // Hats and crash share the metallic oscillators.
            let metal = if self.hat.active() || self.crash.active() {
                self.metal
                    .iter_mut()
                    .zip(METAL)
                    .map(|(o, hz)| o.next_sample(Wave::Square, hz * 1.7 / sr))
                    .sum::<f32>()
                    / 6.0
            } else {
                0.0
            };
            if self.hat.active() {
                let decay = if self.open {
                    p.get(id::OPEN_DECAY)
                } else {
                    p.get(id::HAT_DECAY)
                };
                let env = self.hat.next((-6.9 / (decay * sr)).exp());
                if i % 8 == 0 {
                    self.hat_bp.set(p.get(id::HAT_TONE), 0.3, sr);
                    self.hat_hp.set(6_000.0, 0.0, sr);
                }
                let x = self
                    .hat_bp
                    .process(metal + 0.3 * noise, FilterMode::BandPass);
                let x = self.hat_hp.process(x, FilterMode::HighPass);
                add(x * env * p.get(id::HAT_LEVEL) * 3.0, 0.3);
            }
            if self.crash.active() {
                let env = self
                    .crash
                    .next((-6.9 / (p.get(id::CYMBAL_DECAY) * sr)).exp());
                if i % 8 == 0 {
                    self.crash_hp.set(4_500.0, 0.1, sr);
                }
                let x = self
                    .crash_hp
                    .process(metal * 0.6 + noise * 0.6, FilterMode::HighPass);
                add(x * env * p.get(id::CYMBAL_LEVEL) * 2.0, -0.3);
            }

            if self.rim.active() {
                let age = self.rim.age as f32 / sr;
                let env = self.rim.next(self.coef(0.03));
                let x = sin_turns(age * 1_700.0) + 0.3 * noise;
                add(x * env * p.get(id::PERC_LEVEL), 0.2);
            }
            if self.cowbell.active() {
                let env = self.cowbell.next(self.coef(0.35));
                let x: f32 = self
                    .bell_osc
                    .iter_mut()
                    .zip([540.0, 800.0])
                    .map(|(o, hz)| o.next_sample(Wave::Square, hz / sr))
                    .sum();
                if i % 8 == 0 {
                    self.bell_bp.set(800.0, 0.5, sr);
                }
                let y = self.bell_bp.process(x, FilterMode::BandPass);
                add(y * env * p.get(id::PERC_LEVEL), -0.2);
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

impl AudioNode for Drums {
    fn num_inputs(&self) -> usize {
        0
    }

    fn num_outputs(&self) -> usize {
        2
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        self.p.prepare(sample_rate);
    }

    fn process(&mut self, _ctx: &ProcessContext, io: &mut AudioIo, events: &[Event]) {
        let n = io.frames();
        split_block(
            self,
            n,
            events,
            |s, a, b| s.render(io, a, b),
            |s, e| match *e {
                EventKind::NoteOn { key, vel, .. } => s.note_on(key, vel),
                EventKind::Param { id, val } => s.p.set(id, val),
                EventKind::Cc { cc, val, .. } => {
                    s.controllers.set(cc, val);
                }
                // Drums are one-shots: releasing a key changes nothing.
                EventKind::NoteOff { .. } | EventKind::AllNotesOff => {}
            },
        );
    }

    fn reset(&mut self) {
        let p = self.p.clone();
        let sr = self.sample_rate;
        *self = Self {
            p,
            sample_rate: sr,
            ..Self::default()
        };
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
        "808 style",
        &[
            (id::KICK_TUNE, 45.0),
            (id::KICK_DECAY, 1.2),
            (id::KICK_PUNCH, 0.3),
            (id::KICK_CLICK, 0.1),
            (id::SNARE_TONE, 0.6),
            (id::HAT_DECAY, 0.04),
            (id::CLAP_LEVEL, 0.9),
        ],
    ),
    (
        "Punchy",
        &[
            (id::KICK_TUNE, 60.0),
            (id::KICK_DECAY, 0.3),
            (id::KICK_PUNCH, 0.9),
            (id::KICK_CLICK, 0.7),
            (id::SNARE_TUNE, 220.0),
            (id::SNARE_DECAY, 0.15),
            (id::HAT_TONE, 11_000.0),
        ],
    ),
    (
        "Lo-fi",
        &[
            (id::KICK_DECAY, 0.25),
            (id::SNARE_TONE, 0.8),
            (id::SNARE_TUNE, 150.0),
            (id::HAT_TONE, 4_000.0),
            (id::TOM_TUNE, 90.0),
            (id::VOLUME, 0.7),
        ],
    ),
    (
        "Phonk",
        &[
            (id::KICK_TUNE, 45.0),
            (id::KICK_DECAY, 0.8),
            (id::KICK_PUNCH, 0.5),
            (id::KICK_CLICK, 0.2),
            (id::SNARE_TUNE, 200.0),
            (id::SNARE_TONE, 0.7),
            (id::SNARE_DECAY, 0.18),
            (id::CLAP_LEVEL, 0.9),
            (id::HAT_TONE, 9_000.0),
            (id::HAT_DECAY, 0.035),
            (id::PERC_LEVEL, 0.8),
        ],
    ),
    (
        "Tekk",
        &[
            (id::KICK_TUNE, 55.0),
            (id::KICK_DECAY, 0.35),
            (id::KICK_PUNCH, 1.0),
            (id::KICK_CLICK, 0.8),
            (id::CLAP_LEVEL, 0.8),
            (id::HAT_TONE, 11_000.0),
            (id::OPEN_DECAY, 0.25),
        ],
    ),
    (
        "House",
        &[
            (id::KICK_TUNE, 50.0),
            (id::KICK_DECAY, 0.4),
            (id::KICK_PUNCH, 0.7),
            (id::CLAP_DECAY, 0.3),
            (id::OPEN_DECAY, 0.3),
            (id::HAT_DECAY, 0.04),
        ],
    ),
    (
        // EDM drop kick: about 50 Hz, long and punchy (owner's reference songs).
        "Big room",
        &[
            (id::KICK_TUNE, 50.0),
            (id::KICK_DECAY, 0.35),
            (id::KICK_PUNCH, 1.0),
            (id::KICK_CLICK, 0.6),
            (id::SNARE_TUNE, 210.0),
            (id::SNARE_TONE, 0.5),
            (id::SNARE_DECAY, 0.2),
            (id::CLAP_LEVEL, 0.9),
            (id::CLAP_DECAY, 0.3),
            (id::HAT_TONE, 10_000.0),
            (id::OPEN_DECAY, 0.25),
            (id::CYMBAL_DECAY, 1.5),
        ],
    ),
];
