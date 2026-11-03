//! MIDI keyboard input (midir) and Standard MIDI File import/export (midly).

use std::path::Path;

use midly::num::{u15, u24, u28, u4, u7};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};
use musiton_core::PPQ;

/// Capacity of the MIDI input ring (messages).
const MIDI_RING: usize = 1024;

/// A MIDI input port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiPortInfo {
    /// Stable id (persist this).
    pub id: String,
    /// Display name.
    pub name: String,
}

/// Lists MIDI input ports.
pub fn list_midi_inputs() -> Vec<MidiPortInfo> {
    let Ok(midi) = midir::MidiInput::new("Musiton") else {
        return Vec::new();
    };
    midi.ports()
        .iter()
        .filter_map(|p| {
            Some(MidiPortInfo {
                id: p.id(),
                name: midi.port_name(p).ok()?,
            })
        })
        .collect()
}

/// An open MIDI input. Closing happens when this is dropped.
pub struct MidiInputStream {
    _conn: midir::MidiInputConnection<()>,
    /// Port name.
    pub name: String,
}

/// Opens the port `id` (or the first port) and returns the stream plus the ring the
/// engine reads from (`EngineCommand::SetMidiInput`).
pub fn start_midi_input(
    id: Option<&str>,
) -> Result<(MidiInputStream, rtrb::Consumer<[u8; 3]>), String> {
    let mut midi = midir::MidiInput::new("Musiton").map_err(|e| e.to_string())?;
    midi.ignore(midir::Ignore::All);
    let ports = midi.ports();
    let port = match id {
        Some(id) => midi
            .find_port_by_id(id)
            .ok_or("MIDI device not connected")?,
        None => ports.first().cloned().ok_or("no MIDI input device")?,
    };
    let name = midi.port_name(&port).unwrap_or_else(|_| "MIDI".into());
    let (mut tx, rx) = rtrb::RingBuffer::<[u8; 3]>::new(MIDI_RING);
    let conn = midi
        .connect(
            &port,
            "musiton-in",
            move |_, msg, _| {
                if let [status, rest @ ..] = msg {
                    if (0x80..0xF0).contains(status) {
                        let d1 = rest.first().copied().unwrap_or(0);
                        let d2 = rest.get(1).copied().unwrap_or(0);
                        let _ = tx.push([*status, d1, d2]);
                    }
                }
            },
            (),
        )
        .map_err(|e| e.to_string())?;
    Ok((MidiInputStream { _conn: conn, name }, rx))
}

/// A note in a MIDI file, in Musiton ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmfNote {
    /// Start tick.
    pub start: i64,
    /// Length in ticks.
    pub length: i64,
    /// Key 0..127.
    pub key: u8,
    /// Velocity 1..127.
    pub velocity: u8,
    /// Channel 0..15.
    pub channel: u8,
}

/// One track of a MIDI file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SmfTrack {
    /// Track name (may be empty).
    pub name: String,
    /// Notes sorted by start.
    pub notes: Vec<SmfNote>,
}

/// Errors reading or writing MIDI files.
#[derive(Debug, thiserror::Error)]
pub enum MidiFileError {
    /// File system error.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// Not a valid MIDI file.
    #[error("cannot read MIDI file: {0}")]
    Parse(String),
}

/// Reads a .mid file: tracks with notes, and the first tempo (BPM) if present.
pub fn read_smf(path: &Path) -> Result<(Vec<SmfTrack>, Option<f64>), MidiFileError> {
    let bytes = std::fs::read(path)?;
    let smf = Smf::parse(&bytes).map_err(|e| MidiFileError::Parse(e.to_string()))?;
    let Timing::Metrical(ppq) = smf.header.timing else {
        return Err(MidiFileError::Parse(
            "SMPTE-timed files are not supported".into(),
        ));
    };
    let ppq = i64::from(ppq.as_int().max(1));
    let mut bpm = None;
    let mut out = Vec::new();
    for track in &smf.tracks {
        let mut t = SmfTrack::default();
        let mut now = 0i64;
        let mut open: Vec<(u8, u8, i64, u8)> = Vec::new();
        for ev in track {
            now += i64::from(ev.delta.as_int());
            let tick = now * PPQ / ppq;
            match ev.kind {
                TrackEventKind::Meta(MetaMessage::TrackName(n)) => {
                    t.name = String::from_utf8_lossy(n).into_owned();
                }
                TrackEventKind::Meta(MetaMessage::Tempo(us)) if bpm.is_none() => {
                    bpm = Some(60_000_000.0 / f64::from(us.as_int().max(1)));
                }
                TrackEventKind::Midi { channel, message } => {
                    let ch = channel.as_int();
                    let (key, on_vel) = match message {
                        MidiMessage::NoteOn { key, vel } => (key.as_int(), vel.as_int()),
                        MidiMessage::NoteOff { key, .. } => (key.as_int(), 0),
                        _ => continue,
                    };
                    if on_vel > 0 {
                        open.push((ch, key, tick, on_vel));
                    } else if let Some(i) = open.iter().position(|o| o.0 == ch && o.1 == key) {
                        let (_, _, start, velocity) = open.remove(i);
                        t.notes.push(SmfNote {
                            start,
                            length: (tick - start).max(1),
                            key,
                            velocity,
                            channel: ch,
                        });
                    }
                }
                _ => {}
            }
        }
        if !t.notes.is_empty() {
            t.notes.sort_by_key(|n| (n.start, n.key));
            out.push(t);
        }
    }
    Ok((out, bpm))
}

/// Writes a type-1 .mid file (PPQ 960) with a tempo track and one track per entry.
pub fn write_smf(path: &Path, bpm: f64, tracks: &[SmfTrack]) -> Result<(), MidiFileError> {
    let mut smf = Smf::new(Header::new(
        Format::Parallel,
        Timing::Metrical(u15::new(PPQ as u16)),
    ));
    let us = (60_000_000.0 / bpm.max(1.0)).round() as u32;
    smf.tracks.push(vec![
        TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::new(us))),
        },
        TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        },
    ]);
    for t in tracks {
        // (tick, is_on, note)
        let mut evs: Vec<(i64, bool, SmfNote)> = Vec::new();
        for n in &t.notes {
            evs.push((n.start.max(0), true, *n));
            evs.push((n.start.max(0) + n.length.max(1), false, *n));
        }
        evs.sort_by_key(|(t, on, _)| (*t, *on));
        let mut track = vec![TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::TrackName(t.name.as_bytes())),
        }];
        let mut last = 0i64;
        for (tick, on, n) in evs {
            let delta = u28::new((tick - last).clamp(0, 0x0FFF_FFFF) as u32);
            last = tick;
            let key = u7::new(n.key.min(127));
            let message = if on {
                MidiMessage::NoteOn {
                    key,
                    vel: u7::new(n.velocity.clamp(1, 127)),
                }
            } else {
                MidiMessage::NoteOff {
                    key,
                    vel: u7::new(0),
                }
            };
            track.push(TrackEvent {
                delta,
                kind: TrackEventKind::Midi {
                    channel: u4::new(n.channel.min(15)),
                    message,
                },
            });
        }
        track.push(TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        });
        smf.tracks.push(track);
    }
    smf.save(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_then_import_round_trips() {
        let dir = std::env::temp_dir().join(format!("musiton-midi-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.mid");
        let tracks = vec![
            SmfTrack {
                name: "Piano".into(),
                notes: vec![
                    SmfNote {
                        start: 0,
                        length: 480,
                        key: 60,
                        velocity: 100,
                        channel: 0,
                    },
                    SmfNote {
                        start: 0,
                        length: 960,
                        key: 64,
                        velocity: 90,
                        channel: 0,
                    },
                    SmfNote {
                        start: 960,
                        length: 240,
                        key: 60,
                        velocity: 1,
                        channel: 2,
                    },
                ],
            },
            SmfTrack {
                name: "Bass".into(),
                notes: vec![SmfNote {
                    start: 3840,
                    length: 3840,
                    key: 36,
                    velocity: 127,
                    channel: 1,
                }],
            },
        ];
        write_smf(&path, 97.0, &tracks).unwrap();
        let (back, bpm) = read_smf(&path).unwrap();
        assert_eq!(back, tracks);
        assert!((bpm.unwrap() - 97.0).abs() < 0.01);
        std::fs::write(&path, b"MThd nonsense").unwrap();
        assert!(read_smf(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
