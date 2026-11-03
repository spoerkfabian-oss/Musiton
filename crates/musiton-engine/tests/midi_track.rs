//! MidiTrack: clip notes and live MIDI become sample-accurate events for the instrument.

use std::sync::{Arc, Mutex};

use musiton_core::{TempoMap, Tick, TimeSig, PPQ};
use musiton_engine::node::{AudioIo, AudioNode, Event, EventKind, ProcessContext};
use musiton_engine::nodes::{MidiNote, MidiTrack, RecMidi, MIDI_TRACK_LIVE};

/// Records every event it receives as (absolute sample, kind).
struct Spy(Arc<Mutex<Vec<(u64, EventKind)>>>);

impl AudioNode for Spy {
    fn num_inputs(&self) -> usize {
        0
    }
    fn num_outputs(&self) -> usize {
        2
    }
    fn prepare(&mut self, _: f32, _: usize) {}
    fn process(&mut self, ctx: &ProcessContext, _io: &mut AudioIo, events: &[Event]) {
        let mut log = self.0.lock().unwrap();
        for e in events {
            log.push((ctx.sample_pos + u64::from(e.offset), e.kind));
        }
    }
    fn reset(&mut self) {}
}

fn note(start: i64, len: i64, key: u8) -> MidiNote {
    MidiNote {
        start: Tick(start),
        end: Tick(start + len),
        key,
        vel: 0.8,
        ch: 0,
    }
}

struct Run {
    playing: bool,
    pos: u64,
    frames: usize,
    live: Vec<Event>,
    recording: bool,
}

fn run(track: &mut MidiTrack, runs: &[Run], block: usize) {
    let tempo = TempoMap::constant(120.0);
    track.prepare(48_000.0, block);
    let mut outs = vec![vec![0.0; block]; 2];
    for r in runs {
        let mut done = 0;
        while done < r.frames {
            let n = block.min(r.frames - done);
            let live: &[Event] = if done == 0 { &r.live } else { &[] };
            let ctx = ProcessContext {
                sample_rate: 48_000.0,
                frames: n,
                playing: r.playing,
                sample_pos: r.pos + if r.playing { done as u64 } else { 0 },
                tempo: &tempo,
                time_sig: TimeSig::default(),
                metronome: false,
                inputs: &[],
                recording: r.recording,
                punch: None,
                live_midi: live,
            };
            track.process(&ctx, &mut AudioIo::new(&[], &mut outs, n), &[]);
            done += n;
        }
    }
}

#[test]
fn clip_notes_hit_exact_samples() {
    let log = Arc::new(Mutex::new(Vec::new()));
    // Beat 2 = sample 24 000, half a beat long; then beat 3.
    let notes = vec![note(PPQ, PPQ / 2, 60), note(2 * PPQ, PPQ, 64)];
    let mut t = MidiTrack::new(notes, Box::new(Spy(log.clone())), None);
    run(
        &mut t,
        &[Run {
            playing: true,
            pos: 0,
            frames: 96_000,
            live: vec![],
            recording: false,
        }],
        333,
    );
    let log = log.lock().unwrap();
    assert_eq!(
        *log,
        vec![
            (
                24_000,
                EventKind::NoteOn {
                    ch: 0,
                    key: 60,
                    vel: 0.8
                }
            ),
            (36_000, EventKind::NoteOff { ch: 0, key: 60 }),
            (
                48_000,
                EventKind::NoteOn {
                    ch: 0,
                    key: 64,
                    vel: 0.8
                }
            ),
            (72_000, EventKind::NoteOff { ch: 0, key: 64 }),
        ]
    );
}

#[test]
fn stop_and_jumps_release_notes_and_live_input_reaches_the_instrument() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut t = MidiTrack::new(vec![note(0, 8 * PPQ, 60)], Box::new(Spy(log.clone())), None);
    t.set_param(MIDI_TRACK_LIVE, 1.0);
    let live = vec![Event {
        offset: 0,
        kind: EventKind::NoteOn {
            ch: 0,
            key: 72,
            vel: 1.0,
        },
    }];
    run(
        &mut t,
        &[
            Run {
                playing: true,
                pos: 0,
                frames: 1024,
                live: vec![],
                recording: false,
            },
            Run {
                playing: false,
                pos: 1024,
                frames: 512,
                live: live.clone(),
                recording: false,
            },
            Run {
                playing: true,
                pos: 50_000,
                frames: 512,
                live: vec![],
                recording: false,
            },
        ],
        256,
    );
    let log = log.lock().unwrap();
    let kinds: Vec<EventKind> = log.iter().map(|e| e.1).collect();
    assert_eq!(
        kinds,
        vec![
            EventKind::NoteOn {
                ch: 0,
                key: 60,
                vel: 0.8
            },
            EventKind::AllNotesOff,
            EventKind::NoteOn {
                ch: 0,
                key: 72,
                vel: 1.0
            },
        ]
    );
}

#[test]
fn live_notes_are_recorded_with_their_position() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let (tx, mut rx) = rtrb::RingBuffer::<RecMidi>::new(64);
    let mut t = MidiTrack::new(vec![], Box::new(Spy(log)), Some(tx));
    t.set_param(MIDI_TRACK_LIVE, 1.0);
    let on = Event {
        offset: 0,
        kind: EventKind::NoteOn {
            ch: 0,
            key: 50,
            vel: 0.5,
        },
    };
    let off = Event {
        offset: 0,
        kind: EventKind::NoteOff { ch: 0, key: 50 },
    };
    run(
        &mut t,
        &[
            Run {
                playing: true,
                pos: 1000,
                frames: 256,
                live: vec![on],
                recording: true,
            },
            Run {
                playing: true,
                pos: 1256,
                frames: 256,
                live: vec![off],
                recording: true,
            },
            Run {
                playing: true,
                pos: 1512,
                frames: 256,
                live: vec![on],
                recording: false,
            },
        ],
        256,
    );
    let got: Vec<RecMidi> = std::iter::from_fn(|| rx.pop().ok()).collect();
    assert_eq!(got.len(), 2);
    assert_eq!((got[0].pos, got[0].kind), (1000, on.kind));
    assert_eq!((got[1].pos, got[1].kind), (1256, off.kind));
}

#[test]
fn new_notes_replace_old_ones_without_resetting_and_release_held_notes() {
    use musiton_engine::nodes::MidiNotes;
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut t = MidiTrack::new(vec![note(0, 4 * PPQ, 60)], Box::new(Spy(log.clone())), None);
    run(
        &mut t,
        &[Run {
            playing: true,
            pos: 0,
            frames: 512,
            live: vec![],
            recording: false,
        }],
        256,
    );
    // Note 60 is sounding; replace the notes with one at beat 2.
    let old = t
        .receive(Box::new(MidiNotes(vec![note(PPQ, PPQ, 67)])))
        .unwrap();
    assert!(old.downcast::<MidiNotes>().is_ok());
    run(
        &mut t,
        &[Run {
            playing: true,
            pos: 512,
            frames: 30_000,
            live: vec![],
            recording: false,
        }],
        256,
    );
    let kinds: Vec<EventKind> = log.lock().unwrap().iter().map(|e| e.1).collect();
    assert_eq!(
        kinds,
        vec![
            EventKind::NoteOn {
                ch: 0,
                key: 60,
                vel: 0.8
            },
            EventKind::NoteOff { ch: 0, key: 60 },
            EventKind::NoteOn {
                ch: 0,
                key: 67,
                vel: 0.8
            },
        ]
    );
    // Unknown data is handed back untouched.
    assert!(t.receive(Box::new(5u32)).is_some());
}
