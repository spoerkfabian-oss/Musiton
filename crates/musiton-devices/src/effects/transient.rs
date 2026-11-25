//! Transient shaper: makes the start of each note or hit louder or softer (attack) and
//! its ring-out longer or shorter (sustain), independent of the level.

use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_engine::node::{AudioIo, AudioNode, Event, ProcessContext};

use super::db;
use crate::voices::Params;

/// Most boost or cut (dB) at full attack or sustain.
const MAX_DB: f32 = 15.0;

/// Parameter ids.
#[allow(missing_docs)]
pub mod id {
    pub const ATTACK: u64 = 0;
    pub const SUSTAIN: u64 = 1;
    pub const OUTPUT: u64 = 2;
    pub const MIX: u64 = 3;
}

/// Parameters.
pub const PARAMS: [ParamInfo; 4] = [
    ParamInfo::new(id::ATTACK, "Attack", -1.0, 1.0, 0.0),
    ParamInfo::new(id::SUSTAIN, "Sustain", -1.0, 1.0, 0.0),
    ParamInfo::new(id::OUTPUT, "Output", -24.0, 12.0, 0.0).unit("dB"),
    ParamInfo::new(id::MIX, "Mix", 0.0, 1.0, 1.0),
];

/// Envelope follower with separate attack and release.
#[derive(Debug, Clone, Copy, Default)]
struct Follower {
    value: f32,
    attack: f32,
    release: f32,
}

impl Follower {
    fn new(attack_s: f32, release_s: f32, sr: f32) -> Self {
        Self {
            value: 0.0,
            attack: 1.0 - (-1.0 / (attack_s * sr)).exp(),
            release: 1.0 - (-1.0 / (release_s * sr)).exp(),
        }
    }

    #[inline]
    fn next(&mut self, x: f32) -> f32 {
        let k = if x > self.value {
            self.attack
        } else {
            self.release
        };
        self.value += (x - self.value) * k;
        self.value
    }
}

/// The transient shaper effect.
#[derive(Debug)]
pub struct Transient {
    p: Params<4>,
    /// Fast follower (catches hits), slow-attack follower (lags behind hits) and
    /// long-release follower (holds the ring-out).
    fast: Follower,
    slow: Follower,
    long: Follower,
    sample_rate: f32,
}

impl Default for Transient {
    fn default() -> Self {
        Self {
            p: Params::new(&PARAMS),
            fast: Follower::default(),
            slow: Follower::default(),
            long: Follower::default(),
            sample_rate: 48_000.0,
        }
    }
}

impl AudioNode for Transient {
    fn num_inputs(&self) -> usize {
        2
    }

    fn num_outputs(&self) -> usize {
        2
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate;
        self.p.prepare(sample_rate);
        self.fast = Follower::new(0.0005, 0.05, sample_rate);
        // Both follow the fast envelope (not the signal), so they match it exactly on
        // steady sounds: only rises (hits) and falls (ring-out) make a difference.
        self.slow = Follower::new(0.02, 0.0005, sample_rate);
        self.long = Follower::new(0.0005, 0.4, sample_rate);
    }

    fn process(&mut self, _ctx: &ProcessContext, io: &mut AudioIo, _events: &[Event]) {
        for i in 0..io.frames() {
            self.p.tick();
            let p = &self.p;
            let x = [0, 1].map(|ch| io.input(ch).get(i).copied().unwrap_or(0.0));
            let level = x[0].abs().max(x[1].abs());
            let fast = self.fast.next(level);
            let slow = self.slow.next(fast);
            let long = self.long.next(fast);
            let eps = 1e-6;
            // 0 … 1: how much of the moment is a hit, and how much is ring-out.
            let hit = ((fast - slow) / (fast + eps)).clamp(0.0, 1.0);
            let tail = ((long - fast) / (long + eps)).clamp(0.0, 1.0);
            let gain_db = MAX_DB * (p.get(id::ATTACK) * hit + p.get(id::SUSTAIN) * tail);
            let g = db(gain_db) * db(p.get(id::OUTPUT));
            let mix = p.get(id::MIX);
            for (ch, s) in x.into_iter().enumerate() {
                if let Some(o) = io.output(ch).get_mut(i) {
                    *o = s + (s * g - s) * mix;
                }
            }
        }
    }

    fn reset(&mut self) {
        for f in [&mut self.fast, &mut self.slow, &mut self.long] {
            f.value = 0.0;
        }
    }

    fn params(&self) -> &[ParamInfo] {
        &PARAMS
    }

    fn set_param(&mut self, id: ParamId, value: f32) {
        self.p.set(id, value);
    }
}
