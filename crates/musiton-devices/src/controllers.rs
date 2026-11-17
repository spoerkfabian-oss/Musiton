//! MIDI controllers the built-in instruments react to.

/// Controller numbers.
pub mod cc {
    /// Modulation wheel.
    pub const MOD_WHEEL: u8 = 1;
    /// Channel volume.
    pub const VOLUME: u8 = 7;
    /// Expression (a second volume, for swells).
    pub const EXPRESSION: u8 = 11;
    /// Sustain pedal (down from 0.5).
    pub const SUSTAIN: u8 = 64;
}

/// What a controller change means for the instrument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Nothing the instrument reacts to.
    None,
    /// A value changed (mod wheel, volume, expression).
    Value,
    /// The sustain pedal was pressed.
    PedalDown,
    /// The sustain pedal was released: let go of the notes it held.
    PedalUp,
}

/// Current controller values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Controllers {
    /// Modulation wheel 0..1.
    pub mod_wheel: f32,
    /// Channel volume 0..1.
    pub volume: f32,
    /// Expression 0..1.
    pub expression: f32,
    /// Sustain pedal down.
    pub sustain: bool,
}

impl Default for Controllers {
    fn default() -> Self {
        Self {
            mod_wheel: 0.0,
            volume: 1.0,
            expression: 1.0,
            sustain: false,
        }
    }
}

impl Controllers {
    /// Applies controller `number` = `val` (0..1).
    pub fn set(&mut self, number: u8, val: f32) -> Change {
        let val = val.clamp(0.0, 1.0);
        match number {
            cc::MOD_WHEEL => self.mod_wheel = val,
            cc::VOLUME => self.volume = val,
            cc::EXPRESSION => self.expression = val,
            cc::SUSTAIN => {
                let down = val >= 0.5;
                let was = std::mem::replace(&mut self.sustain, down);
                return match (was, down) {
                    (false, true) => Change::PedalDown,
                    (true, false) => Change::PedalUp,
                    _ => Change::None,
                };
            }
            _ => return Change::None,
        }
        Change::Value
    }

    /// Gain from volume and expression (squared, like most MIDI instruments).
    pub fn gain(&self) -> f32 {
        let g = self.volume * self.expression;
        g * g
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pedal_edges_and_gain() {
        let mut c = Controllers::default();
        assert_eq!(c.gain(), 1.0);
        assert_eq!(c.set(cc::SUSTAIN, 1.0), Change::PedalDown);
        assert_eq!(c.set(cc::SUSTAIN, 0.9), Change::None);
        assert_eq!(c.set(cc::SUSTAIN, 0.0), Change::PedalUp);
        assert_eq!(c.set(cc::VOLUME, 0.5), Change::Value);
        assert_eq!(c.gain(), 0.25);
        assert_eq!(c.set(74, 0.3), Change::None);
    }
}
