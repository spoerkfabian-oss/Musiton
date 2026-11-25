//! Shared parts of the built-in instruments: voice allocation (with stealing and the
//! sustain pedal), smoothed parameters and splitting a block at its events.

use musiton_core::ids::ParamId;
use musiton_core::params::{ParamInfo, ParamKind};
use musiton_engine::node::{Event, EventKind};

use crate::controllers::{Change, Controllers};

/// One sound of a polyphonic instrument.
pub trait Voice: Clone + Default {
    /// The key was released (or the pedal let go of it).
    fn release(&mut self);
    /// Whether it still makes sound.
    fn is_active(&self) -> bool;
}

#[derive(Debug, Clone, Default)]
struct Slot<V> {
    voice: V,
    key: Option<u8>,
    age: u64,
    /// The key was released while the sustain pedal was down.
    pedal_held: bool,
}

/// A fixed set of voices with note allocation and MIDI controllers.
#[derive(Debug, Clone, Default)]
pub struct Voices<V> {
    slots: Vec<Slot<V>>,
    clock: u64,
    /// Mod wheel, volume, expression and sustain pedal.
    pub controllers: Controllers,
}

impl<V: Voice> Voices<V> {
    /// Allocates `count` voices (off the audio thread).
    pub fn prepare(&mut self, count: usize) {
        self.slots = vec![Slot::default(); count];
    }

    /// Picks a voice for `key`: a silent one, else the oldest released one, else the
    /// oldest. The caller starts it.
    pub fn note_on(&mut self, key: u8) -> Option<&mut V> {
        self.clock += 1;
        let i = self
            .slots
            .iter()
            .position(|s| !s.voice.is_active())
            .or_else(|| {
                self.slots
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.key.is_none())
                    .min_by_key(|(_, s)| s.age)
                    .map(|(i, _)| i)
            })
            .or_else(|| {
                self.slots
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, s)| s.age)
                    .map(|(i, _)| i)
            })?;
        let slot = self.slots.get_mut(i)?;
        slot.key = Some(key);
        slot.age = self.clock;
        slot.pedal_held = false;
        Some(&mut slot.voice)
    }

    /// Releases the voices of `key` (or keeps them while the sustain pedal is down).
    pub fn note_off(&mut self, key: u8) {
        let sustain = self.controllers.sustain;
        for s in self
            .slots
            .iter_mut()
            .filter(|s| s.key == Some(key) && !s.pedal_held)
        {
            if sustain {
                s.pedal_held = true;
            } else {
                s.key = None;
                s.voice.release();
            }
        }
    }

    /// Releases the voices the pedal holds (every held key with `all`).
    pub fn release_held(&mut self, all: bool) {
        for s in self
            .slots
            .iter_mut()
            .filter(|s| s.key.is_some() && (all || s.pedal_held))
        {
            s.key = None;
            s.pedal_held = false;
            s.voice.release();
        }
    }

    /// Handles a MIDI controller.
    pub fn controller(&mut self, number: u8, val: f32) {
        if self.controllers.set(number, val) == Change::PedalUp {
            self.release_held(false);
        }
    }

    /// Whether a key is down (or held by the pedal).
    pub fn any_held(&self) -> bool {
        self.slots.iter().any(|s| s.key.is_some())
    }

    /// Every voice that sounds.
    pub fn active(&mut self) -> impl Iterator<Item = &mut V> {
        self.slots
            .iter_mut()
            .map(|s| &mut s.voice)
            .filter(|v| v.is_active())
    }

    /// Every voice.
    pub fn all(&mut self) -> impl Iterator<Item = &mut V> {
        self.slots.iter_mut().map(|s| &mut s.voice)
    }
}

/// Parameter values with per-sample smoothing of continuous knobs (no zipper noise or
/// clicks when a knob jumps). Whole-number, switch and choice values change at once.
#[derive(Debug, Clone)]
pub struct Params<const N: usize> {
    info: &'static [ParamInfo; N],
    target: [f32; N],
    value: [f32; N],
    coef: f32,
}

impl<const N: usize> Params<N> {
    /// Defaults of `info`.
    pub fn new(info: &'static [ParamInfo; N]) -> Self {
        let target = info.map(|p| p.default);
        Self {
            info,
            target,
            value: target,
            coef: 0.0,
        }
    }

    /// Sets the smoothing for `sample_rate` (about 20 ms) and jumps to the targets.
    pub fn prepare(&mut self, sample_rate: f32) {
        self.coef = (-1.0 / (0.005 * sample_rate)).exp();
        self.value = self.target;
    }

    /// Sets parameter `id` (clamped to its range).
    pub fn set(&mut self, id: ParamId, v: f32) {
        let i = id.0 as usize;
        if let (Some(t), Some(info)) = (self.target.get_mut(i), self.info.get(i)) {
            *t = v.clamp(info.min.min(info.max), info.max.max(info.min));
            if !matches!(info.kind, ParamKind::Linear | ParamKind::Log) {
                if let Some(x) = self.value.get_mut(i) {
                    *x = *t;
                }
            }
        }
    }

    /// Moves every value one sample towards its target.
    #[inline]
    pub fn tick(&mut self) {
        for (v, t) in self.value.iter_mut().zip(&self.target) {
            *v = t + (*v - t) * self.coef;
        }
    }

    /// Current (smoothed) value of `id`.
    #[inline]
    pub fn get(&self, id: u64) -> f32 {
        self.value.get(id as usize).copied().unwrap_or(0.0)
    }

    /// Target value of `id` (what the knob shows).
    #[inline]
    pub fn target(&self, id: u64) -> f32 {
        self.target.get(id as usize).copied().unwrap_or(0.0)
    }

    /// Choice or whole-number value of `id` as an index.
    #[inline]
    pub fn index(&self, id: u64) -> usize {
        self.target(id).round().max(0.0) as usize
    }
}

/// Renders the stretches of a block between its events and hands each event over in
/// order: `render(node, from, to)` and `event(node, kind)`.
pub fn split_block<N: ?Sized>(
    node: &mut N,
    frames: usize,
    events: &[Event],
    mut render: impl FnMut(&mut N, usize, usize),
    mut event: impl FnMut(&mut N, &EventKind),
) {
    let mut pos = 0;
    for e in events {
        let at = (e.offset as usize).min(frames);
        render(node, pos, at);
        pos = at;
        event(node, &e.kind);
    }
    render(node, pos, frames);
}

/// Applies note-offs, controllers and parameter changes; returns a note-on (key,
/// velocity 0..1) for the instrument to start.
pub fn handle_event<V: Voice, const N: usize>(
    voices: &mut Voices<V>,
    params: &mut Params<N>,
    event: &EventKind,
) -> Option<(u8, f32)> {
    match *event {
        EventKind::NoteOn { key, vel, .. } => return Some((key, vel)),
        EventKind::NoteOff { key, .. } => voices.note_off(key),
        EventKind::AllNotesOff => {
            voices.controllers.sustain = false;
            voices.release_held(true);
        }
        EventKind::Param { id, val } => params.set(id, val),
        EventKind::Cc { cc, val, .. } => voices.controller(cc, val),
    }
    None
}

/// Frequency of MIDI note `pitch` (fractional) in Hz.
#[inline]
pub fn note_hz(pitch: f32) -> f32 {
    440.0 * ((pitch - 69.0) / 12.0).exp2()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Default)]
    struct V {
        on: bool,
    }

    impl Voice for V {
        fn release(&mut self) {
            self.on = false;
        }
        fn is_active(&self) -> bool {
            self.on
        }
    }

    #[test]
    fn voices_are_stolen_oldest_first_and_the_pedal_holds_keys() {
        let mut v = Voices::<V>::default();
        v.prepare(2);
        for key in [60, 62, 64] {
            v.note_on(key).unwrap().on = true;
        }
        assert_eq!(v.active().count(), 2);
        v.controller(crate::controllers::cc::SUSTAIN, 1.0);
        v.note_off(64);
        assert_eq!(v.active().count(), 2, "pedal holds");
        v.controller(crate::controllers::cc::SUSTAIN, 0.0);
        assert_eq!(v.active().count(), 1);
        assert!(v.any_held());
    }

    #[test]
    fn continuous_params_glide_and_choices_jump() {
        static INFO: [ParamInfo; 2] = [
            ParamInfo::new(0, "Level", 0.0, 1.0, 0.0),
            ParamInfo::choice(1, "Mode", &["A", "B"], 0),
        ];
        let mut p = Params::new(&INFO);
        p.prepare(48_000.0);
        p.set(ParamId(0), 1.0);
        p.set(ParamId(1), 1.0);
        p.tick();
        assert!(p.get(0) > 0.0 && p.get(0) < 0.1);
        assert_eq!(p.index(1), 1);
        for _ in 0..4_800 {
            p.tick();
        }
        assert!((p.get(0) - 1.0).abs() < 1e-3);
        p.set(ParamId(0), 5.0);
        assert_eq!(p.target(0), 1.0, "clamped");
    }
}
