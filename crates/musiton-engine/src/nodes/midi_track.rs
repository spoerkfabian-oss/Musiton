//! A MIDI track: turns clip notes and live MIDI into events for its instrument.

use musiton_core::ids::ParamId;
use musiton_core::Tick;

use crate::node::{AudioIo, AudioNode, Event, EventKind, ParamInfo, ProcessContext};

/// Parameter: 1 = this track receives live MIDI (keyboard, preview), 0 = not.
pub const MIDI_TRACK_LIVE: ParamId = ParamId(100);
/// Most events passed to the instrument per block.
const MAX_EVENTS: usize = 1024;

const PARAMS: [ParamInfo; 1] = [ParamInfo::toggle(MIDI_TRACK_LIVE.0, "Live input", false)];

/// A note on the timeline (absolute ticks, already cut to its clip).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MidiNote {
    /// Start.
    pub start: Tick,
    /// End (exclusive).
    pub end: Tick,
    /// Key 0..127.
    pub key: u8,
    /// Velocity 0..1.
    pub vel: f32,
    /// Channel 0..15.
    pub ch: u8,
}

/// A controller change on the timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MidiCc {
    /// Position.
    pub tick: Tick,
    /// Controller number 0..127.
    pub cc: u8,
    /// Value 0..1.
    pub val: f32,
    /// Channel 0..15.
    pub ch: u8,
}

/// Value a controller returns to when a clip that moved it no longer sets it.
fn cc_default(cc: u8) -> f32 {
    match cc {
        7 | 11 => 1.0, // volume, expression
        10 => 0.5,     // pan
        _ => 0.0,
    }
}

/// A live MIDI event captured while recording.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecMidi {
    /// Song position (samples).
    pub pos: u64,
    /// What was played.
    pub kind: EventKind,
}

/// Plays `notes` (sorted by start) and, when live, the incoming MIDI into an instrument.
/// Outputs whatever the instrument outputs (stereo).
pub struct MidiTrack {
    notes: Vec<MidiNote>,
    ccs: Vec<MidiCc>,
    /// Controllers that clip playback has set (bit per controller number).
    cc_touched: [u64; 2],
    instrument: Box<dyn AudioNode>,
    events: Vec<Event>,
    live: bool,
    rec: Option<rtrb::Producer<RecMidi>>,
    /// Position the next block must start at to count as continuous playback.
    expected: Option<u64>,
    /// Notes started by clips that are still sounding: bit `ch` of `sounding[key]`.
    sounding: [u16; 128],
    /// New notes arrived: release what clips are still holding.
    release_clip_notes: bool,
}

/// New notes and controller changes for a [`MidiTrack`] (sent with
/// `EngineCommand::NodeData`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MidiNotes {
    /// Notes.
    pub notes: Vec<MidiNote>,
    /// Controller changes.
    pub ccs: Vec<MidiCc>,
}

impl MidiTrack {
    /// Track playing `notes` into `instrument`; live MIDI is sent to `rec` while recording.
    pub fn new(
        mut notes: Vec<MidiNote>,
        instrument: Box<dyn AudioNode>,
        rec: Option<rtrb::Producer<RecMidi>>,
    ) -> Self {
        notes.sort_by_key(|n| n.start);
        Self {
            notes,
            ccs: Vec::new(),
            cc_touched: [0; 2],
            instrument,
            events: Vec::new(),
            live: false,
            rec,
            expected: None,
            sounding: [0; 128],
            release_clip_notes: false,
        }
    }

    /// Also plays the controller changes `ccs`.
    pub fn with_ccs(mut self, mut ccs: Vec<MidiCc>) -> Self {
        ccs.sort_by_key(|c| c.tick);
        self.ccs = ccs;
        self
    }

    /// Playback (re)starts at `pos`: sets every controller the clips use to the value it
    /// has there. Controllers the clips moved earlier but do not set before `pos` return
    /// to their default.
    fn chase_controllers(&mut self, pos: u64, at: impl Fn(Tick) -> u64) {
        let mut last = [(f32::NAN, 0u8); 128];
        for c in &self.ccs {
            if at(c.tick) >= pos {
                break;
            }
            if let Some(slot) = last.get_mut(usize::from(c.cc)) {
                *slot = (c.val, c.ch);
            }
        }
        for cc in 0..128u8 {
            let (val, ch) = last.get(usize::from(cc)).copied().unwrap_or((f32::NAN, 0));
            let touched = self
                .cc_touched
                .get(usize::from(cc / 64))
                .is_some_and(|w| w & (1 << (cc % 64)) != 0);
            if !val.is_nan() {
                self.touch(cc);
                self.push(0, EventKind::Cc { ch, cc, val });
            } else if touched {
                let val = cc_default(cc);
                self.push(0, EventKind::Cc { ch: 0, cc, val });
            }
        }
    }

    fn touch(&mut self, cc: u8) {
        if let Some(w) = self.cc_touched.get_mut(usize::from(cc / 64)) {
            *w |= 1 << (cc % 64);
        }
    }

    fn push(&mut self, offset: usize, kind: EventKind) {
        if self.events.len() < self.events.capacity() {
            self.events.push(Event {
                offset: offset as u32,
                kind,
            });
        }
    }
}

impl AudioNode for MidiTrack {
    fn num_inputs(&self) -> usize {
        0
    }

    fn num_outputs(&self) -> usize {
        self.instrument.num_outputs()
    }

    fn prepare(&mut self, sample_rate: f32, max_block: usize) {
        self.events = Vec::with_capacity(MAX_EVENTS);
        self.instrument.prepare(sample_rate, max_block);
    }

    fn process(&mut self, ctx: &ProcessContext, io: &mut AudioIo, _events: &[Event]) {
        self.events.clear();
        let pos = ctx.sample_pos;
        let n = ctx.frames as u64;
        let continuous = ctx.playing && self.expected == Some(pos);
        if self.expected.is_some() && !continuous {
            self.push(0, EventKind::AllNotesOff);
            self.sounding = [0; 128];
        }
        if std::mem::take(&mut self.release_clip_notes) {
            for key in 0..128u8 {
                let bits = self.sounding.get(usize::from(key)).copied().unwrap_or(0);
                for ch in 0..16u8 {
                    if bits & (1 << ch) != 0 {
                        self.push(0, EventKind::NoteOff { ch, key });
                    }
                }
            }
            self.sounding = [0; 128];
        }
        self.expected = ctx.playing.then_some(pos + n);

        if ctx.playing {
            let sr = f64::from(ctx.sample_rate);
            let at = |t: Tick| ctx.tempo.tick_to_sample(t.0 as f64, sr).round() as u64;
            let end_tick = ctx.tempo.sample_to_tick((pos + n) as f64, sr).ceil() as i64 + 1;
            if !continuous {
                self.chase_controllers(pos, at);
            }
            for i in 0..self.ccs.len() {
                let Some(c) = self.ccs.get(i).copied() else {
                    break;
                };
                if c.tick.0 > end_tick {
                    break;
                }
                let s = at(c.tick);
                if (pos..pos + n).contains(&s) {
                    self.touch(c.cc);
                    self.push(
                        (s - pos) as usize,
                        EventKind::Cc {
                            ch: c.ch,
                            cc: c.cc,
                            val: c.val,
                        },
                    );
                }
            }
            for i in 0..self.notes.len() {
                let Some(note) = self.notes.get(i).copied() else {
                    break;
                };
                if note.start.0 > end_tick {
                    break;
                }
                let (s, e) = (at(note.start), at(note.end));
                if e <= s {
                    continue;
                }
                if (pos..pos + n).contains(&e) {
                    if let Some(b) = self.sounding.get_mut(usize::from(note.key)) {
                        *b &= !(1u16 << (note.ch & 15));
                    }
                    self.push(
                        (e - pos) as usize,
                        EventKind::NoteOff {
                            ch: note.ch,
                            key: note.key,
                        },
                    );
                }
                let bit = 1u16 << (note.ch & 15);
                // Started from (or jumped to) the middle of a note: play it from here.
                let chased = !continuous && s < pos && e > pos;
                if chased || (pos..pos + n).contains(&s) {
                    self.push(
                        s.saturating_sub(pos) as usize,
                        EventKind::NoteOn {
                            ch: note.ch,
                            key: note.key,
                            vel: note.vel,
                        },
                    );
                    if let Some(b) = self.sounding.get_mut(usize::from(note.key)) {
                        *b |= bit;
                    }
                }
            }
        }

        if self.live {
            for e in ctx.live_midi {
                self.push(e.offset as usize, e.kind);
                if ctx.recording {
                    if let Some(tx) = self.rec.as_mut() {
                        let _ = tx.push(RecMidi {
                            pos: pos + u64::from(e.offset),
                            kind: e.kind,
                        });
                    }
                }
            }
        }

        // Offs before ons at the same sample, so a repeated note retriggers.
        self.events
            .sort_unstable_by_key(|e| (e.offset, matches!(e.kind, EventKind::NoteOn { .. })));
        self.instrument.process(ctx, io, &self.events);
    }

    fn reset(&mut self) {
        self.expected = None;
        self.instrument.reset();
    }

    fn params(&self) -> &[ParamInfo] {
        &PARAMS
    }

    fn set_param(&mut self, id: ParamId, value: f32) {
        if id == MIDI_TRACK_LIVE {
            self.live = value > 0.5;
        } else {
            self.instrument.set_param(id, value);
        }
    }

    fn latency(&self) -> u32 {
        self.instrument.latency()
    }

    fn receive(
        &mut self,
        data: Box<dyn std::any::Any + Send>,
    ) -> Option<Box<dyn std::any::Any + Send>> {
        match data.downcast::<MidiNotes>() {
            Ok(mut new) => {
                new.notes.sort_unstable_by_key(|n| n.start);
                new.ccs.sort_unstable_by_key(|c| c.tick);
                std::mem::swap(&mut self.notes, &mut new.notes);
                std::mem::swap(&mut self.ccs, &mut new.ccs);
                self.release_clip_notes = true;
                // Re-chase the controllers on the next block.
                self.expected = None;
                Some(new)
            }
            Err(other) => self.instrument.receive(other),
        }
    }
}
