//! ADSR envelope.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// Attack/decay/sustain/release envelope (linear attack, exponential decay and release).
/// Retriggering starts the attack from the current level, so it never jumps.
#[derive(Debug, Clone)]
pub struct Adsr {
    stage: Stage,
    level: f32,
    attack_step: f32,
    decay_coef: f32,
    sustain: f32,
    release_coef: f32,
}

impl Default for Adsr {
    fn default() -> Self {
        Self {
            stage: Stage::Idle,
            level: 0.0,
            attack_step: 1.0,
            decay_coef: 0.0,
            sustain: 1.0,
            release_coef: 0.0,
        }
    }
}

/// Exponential coefficient that covers ~99 % of the distance in `secs`.
fn coef(secs: f32, sample_rate: f32) -> f32 {
    (-4.6 / (secs.max(0.0005) * sample_rate)).exp()
}

impl Adsr {
    /// Sets times (seconds) and sustain level (0..1).
    pub fn set(&mut self, attack: f32, decay: f32, sustain: f32, release: f32, sample_rate: f32) {
        self.attack_step = 1.0 / (attack.max(0.0005) * sample_rate);
        self.decay_coef = coef(decay, sample_rate);
        self.sustain = sustain.clamp(0.0, 1.0);
        self.release_coef = coef(release, sample_rate);
    }

    /// Starts (or restarts) the envelope.
    pub fn gate_on(&mut self) {
        self.stage = Stage::Attack;
    }

    /// Releases the envelope.
    pub fn gate_off(&mut self) {
        if self.stage != Stage::Idle {
            self.stage = Stage::Release;
        }
    }

    /// `true` while the envelope produces output.
    pub fn is_active(&self) -> bool {
        self.stage != Stage::Idle
    }

    /// `true` after `gate_off` until silent.
    pub fn is_releasing(&self) -> bool {
        self.stage == Stage::Release
    }

    /// Current level.
    pub fn level(&self) -> f32 {
        self.level
    }

    /// Next envelope value.
    #[inline]
    pub fn next_value(&mut self) -> f32 {
        match self.stage {
            Stage::Idle => self.level = 0.0,
            Stage::Attack => {
                self.level += self.attack_step;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.level = self.sustain + (self.level - self.sustain) * self.decay_coef;
                if (self.level - self.sustain).abs() < 1e-4 {
                    self.level = self.sustain;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => self.level = self.sustain,
            Stage::Release => {
                self.level *= self.release_coef;
                if self.level < 1e-4 {
                    self.level = 0.0;
                    self.stage = Stage::Idle;
                }
            }
        }
        self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_have_their_lengths() {
        let mut e = Adsr::default();
        e.set(0.01, 0.1, 0.5, 0.2, 48_000.0);
        e.gate_on();
        let attack = (0..10_000).position(|_| e.next_value() >= 1.0).unwrap();
        assert!((470..=490).contains(&attack), "attack {attack}");
        for _ in 0..9_600 {
            e.next_value();
        }
        assert!((e.level() - 0.5).abs() < 0.01);
        // Release time = time to fall to 1 % of the level at release.
        e.gate_off();
        for _ in 0..9_600 {
            e.next_value();
        }
        assert!((e.level() - 0.005).abs() < 0.0005, "level {}", e.level());
    }

    #[test]
    fn retrigger_continues_from_current_level() {
        let mut e = Adsr::default();
        e.set(0.05, 0.1, 0.8, 0.5, 48_000.0);
        e.gate_on();
        for _ in 0..1000 {
            e.next_value();
        }
        e.gate_off();
        let before = e.next_value();
        e.gate_on();
        let after = e.next_value();
        assert!(after >= before && after - before < 0.001);
    }
}
