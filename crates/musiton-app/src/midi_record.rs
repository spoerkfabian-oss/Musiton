//! Turning recorded live MIDI into a MIDI clip.

use std::collections::HashMap;

use musiton_core::ids::{ClipId, TrackId};
use musiton_core::Tick;
use musiton_engine::node::EventKind;
use musiton_engine::nodes::RecMidi;
use musiton_project::{CcEvent, Clip, Edit, MidiClip, Note, Project};

/// Builds the edit that adds a clip with the recorded notes and controller moves to
/// `track`. Positions are moved `latency` frames earlier; notes still held at the end
/// close at `stop`. The clip spans whole bars. In replace mode (not `overdub`), notes of
/// other clips on the track that start inside the recorded range are removed, and so are
/// their changes of the controllers that were recorded.
pub fn to_edit(
    project: &mut Project,
    track: TrackId,
    events: &[RecMidi],
    latency: i64,
    stop: u64,
    sample_rate: f64,
    overdub: bool,
) -> Option<Edit> {
    project.track(track)?;
    let tick = |pos: u64| {
        let s = (pos as i64 - latency).max(0) as f64;
        Tick(project.tempo.sample_to_tick(s, sample_rate).round() as i64)
    };
    let mut open: HashMap<(u8, u8), (Tick, u8)> = HashMap::new();
    let mut notes: Vec<(Tick, Tick, u8, u8, u8)> = Vec::new();
    // (position, controller, value, channel); repeated values are dropped.
    let mut ccs: Vec<(Tick, u8, u8, u8)> = Vec::new();
    let mut last_cc: HashMap<(u8, u8), u8> = HashMap::new();
    for e in events {
        match e.kind {
            EventKind::NoteOn { ch, key, vel } => {
                let v = (vel * 127.0).round().clamp(1.0, 127.0) as u8;
                open.insert((ch, key), (tick(e.pos), v));
            }
            EventKind::NoteOff { ch, key } => {
                if let Some((start, v)) = open.remove(&(ch, key)) {
                    notes.push((start, tick(e.pos).max(Tick(start.0 + 1)), key, v, ch));
                }
            }
            EventKind::Cc { ch, cc, val } => {
                let value = (val * 127.0).round().clamp(0.0, 127.0) as u8;
                if last_cc.insert((ch, cc), value) != Some(value) {
                    ccs.push((tick(e.pos), cc, value, ch));
                }
            }
            _ => {}
        }
    }
    let end_all = tick(stop);
    for ((ch, key), (start, v)) in open {
        notes.push((start, end_all.max(Tick(start.0 + 1)), key, v, ch));
    }
    if notes.is_empty() && ccs.is_empty() {
        return None;
    }
    let bar = project.time_sig.ticks_per_bar();
    let starts = notes
        .iter()
        .map(|n| n.0 .0)
        .chain(ccs.iter().map(|c| c.0 .0));
    let ends = notes
        .iter()
        .map(|n| n.1 .0)
        .chain(ccs.iter().map(|c| c.0 .0 + 1));
    let first = starts.min()?;
    let last = ends.max()?;
    let start = first.div_euclid(bar) * bar;
    let end = (last + bar - 1).div_euclid(bar) * bar;

    let mut edits = Vec::new();
    if !overdub {
        let t = project.track(track)?;
        for c in &t.clips {
            if let Clip::Midi(m) = c {
                let mut kept = m.clone();
                kept.notes.retain(|n| {
                    let abs = m.start.0 + n.start.0;
                    !(start..end).contains(&abs)
                });
                kept.cc.retain(|c| {
                    let abs = m.start.0 + c.tick.0;
                    !((start..end).contains(&abs)
                        && last_cc.contains_key(&(c.channel, c.controller)))
                });
                if kept.notes.len() != m.notes.len() || kept.cc.len() != m.cc.len() {
                    edits.push(Edit::ReplaceClip {
                        track,
                        clip: Clip::Midi(kept),
                    });
                }
            }
        }
    }
    let mut clip_notes: Vec<Note> = notes
        .iter()
        .map(|&(s, e, key, velocity, channel)| Note {
            pitch: key,
            velocity,
            start: Tick(s.0 - start),
            length: Tick(e.0 - s.0),
            channel,
        })
        .collect();
    clip_notes.sort_by_key(|n| (n.start, n.pitch));
    edits.push(Edit::AddClip {
        track,
        clip: Some(Clip::Midi(MidiClip {
            id: ClipId(project.alloc_id()),
            start: Tick(start),
            length: Tick(end - start),
            notes: clip_notes,
            cc: ccs
                .iter()
                .map(|&(t, controller, value, channel)| CcEvent {
                    tick: Tick(t.0 - start),
                    controller,
                    value,
                    channel,
                })
                .collect(),
        })),
    });
    Some(Edit::Batch(edits))
}

#[cfg(test)]
mod tests {
    use super::*;
    use musiton_core::{TempoMap, PPQ};
    use musiton_project::{Track, TrackKind, UndoStack};

    fn ev(pos: u64, on: bool, key: u8) -> RecMidi {
        RecMidi {
            pos,
            kind: if on {
                EventKind::NoteOn {
                    ch: 0,
                    key,
                    vel: 1.0,
                }
            } else {
                EventKind::NoteOff { ch: 0, key }
            },
        }
    }

    fn project() -> Project {
        let mut p = Project {
            tempo: TempoMap::constant(120.0),
            ..Project::default()
        };
        p.tracks.push(Track {
            id: TrackId(1),
            kind: TrackKind::Instrument,
            ..Track::default()
        });
        p
    }

    #[test]
    fn notes_land_on_their_ticks_after_latency() {
        let mut p = project();
        // Played at beat 5 (bar 2) + 256 frames latency, held one beat; key 64 never released.
        let events = [
            ev(96_256, true, 60),
            ev(120_256, false, 60),
            ev(120_256, true, 64),
        ];
        let edit = to_edit(&mut p, TrackId(1), &events, 256, 144_256, 48_000.0, true).unwrap();
        UndoStack::new().perform(&mut p, edit, false).unwrap();
        let Clip::Midi(m) = &p.tracks[0].clips[0] else {
            panic!()
        };
        assert_eq!((m.start, m.length), (Tick(4 * PPQ), Tick(4 * PPQ)));
        assert_eq!(m.notes[0].start, Tick(0));
        assert_eq!(m.notes[0].length, Tick(PPQ));
        assert_eq!(
            (m.notes[1].pitch, m.notes[1].start, m.notes[1].length),
            (64, Tick(PPQ), Tick(PPQ))
        );
        assert_eq!(m.notes[0].velocity, 127);
    }

    #[test]
    fn replace_mode_removes_old_notes_in_range() {
        let mut p = project();
        let mut u = UndoStack::new();
        let e1 = to_edit(
            &mut p,
            TrackId(1),
            &[ev(0, true, 60), ev(1000, false, 60)],
            0,
            2000,
            48_000.0,
            true,
        )
        .unwrap();
        u.perform(&mut p, e1, false).unwrap();
        let e2 = to_edit(
            &mut p,
            TrackId(1),
            &[ev(500, true, 62), ev(900, false, 62)],
            0,
            2000,
            48_000.0,
            false,
        )
        .unwrap();
        u.perform(&mut p, e2, false).unwrap();
        let counts: Vec<usize> = p.tracks[0]
            .clips
            .iter()
            .map(|c| match c {
                Clip::Midi(m) => m.notes.len(),
                Clip::Audio(_) => 0,
            })
            .collect();
        assert_eq!(counts, vec![0, 1]);
        assert!(to_edit(&mut p, TrackId(1), &[], 0, 0, 48_000.0, false).is_none());
    }

    #[test]
    fn controller_moves_are_recorded_and_repeats_dropped() {
        let mut p = project();
        let cc = |pos: u64, val: f32| RecMidi {
            pos,
            kind: EventKind::Cc { ch: 0, cc: 1, val },
        };
        // One beat = 24 000 samples at 120 BPM / 48 kHz.
        let events = [
            ev(0, true, 60),
            cc(24_000, 0.5),
            cc(30_000, 0.5),
            cc(48_000, 1.0),
            ev(60_000, false, 60),
        ];
        let edit = to_edit(&mut p, TrackId(1), &events, 0, 96_000, 48_000.0, false).unwrap();
        UndoStack::default().perform(&mut p, edit, false).unwrap();
        let Clip::Midi(m) = &p.tracks[0].clips[0] else {
            panic!("no MIDI clip");
        };
        let got: Vec<(i64, u8, u8)> =
            m.cc.iter()
                .map(|c| (c.tick.0, c.controller, c.value))
                .collect();
        assert_eq!(got, [(PPQ, 1, 64), (2 * PPQ, 1, 127)]);
        assert_eq!(m.notes.len(), 1);
    }
}
