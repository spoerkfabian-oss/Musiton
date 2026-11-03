//! A MIDI track: turns clip notes and live MIDI into events for its instrument.

use musiton_core::ids::ParamId;
use musiton_core::Tick;

use crate::node::{AudioIo, AudioNode, Event, EventKind, ParamInfo, ProcessContext};

/// Parameter: 1 = this track receives live MIDI (keyboard, preview), 0 = not.
pub const MIDI_TRACK_LIVE: ParamId = ParamId(100);
/// Most events passed to the instrument per block.
const MAX_EVENTS: usize = 1024;

const PARAMS: [ParamInfo; 1] = [ParamInfo {
    id: MIDI_TRACK_LIVE,
    name: "Live input",
    min: 0.0,
    max: 1.0,
    default: 0.0,
}];

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
    instrument: Box<dyn AudioNode>,
    events: Vec<Event>,
    live: bool,
    rec: Option<rtrb::Producer<RecMidi>>,
    /// Position the next block must start at to count as continuous playback.
    expected: Option<u64>,
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
            instrument,
            events: Vec::new(),
            live: false,
            rec,
            expected: None,
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
        }
        self.expected = ctx.playing.then_some(pos + n);

        if ctx.playing {
            let sr = f64::from(ctx.sample_rate);
            let at = |t: Tick| ctx.tempo.tick_to_sample(t.0 as f64, sr).round() as u64;
            let end_tick = ctx.tempo.sample_to_tick((pos + n) as f64, sr).ceil() as i64 + 1;
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
                    self.push(
                        (e - pos) as usize,
                        EventKind::NoteOff {
                            ch: note.ch,
                            key: note.key,
                        },
                    );
                }
                if (pos..pos + n).contains(&s) {
                    self.push(
                        (s - pos) as usize,
                        EventKind::NoteOn {
                            ch: note.ch,
                            key: note.key,
                            vel: note.vel,
                        },
                    );
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
}
