//! Undo/redo and save/load round trips.

use musiton_core::ids::{AssetId, ClipId, MarkerId, TrackId};
use musiton_core::{SamplePos, TempoMap, TempoPoint, Tick, TimeSig};
use musiton_project::{
    load, project_dir_for, save, AudioClip, ChannelStrip, Clip, Edit, Marker, MidiClip, Note,
    Project, ProjectError, Track, TrackKind, UndoStack,
};
use proptest::prelude::*;

/// A random user action; turned into an [`Edit`] against the current project.
#[derive(Debug, Clone)]
enum Op {
    AddTrack(usize, u8),
    RemoveTrack(usize),
    MoveTrack(usize, usize),
    Rename(usize, String),
    Color(usize, [u8; 3]),
    Strip(Option<usize>, f32, f32, bool, bool),
    AddAudioClip(usize, i64, i64),
    AddMidiClip(usize, i64, Vec<(u8, u8, i64, i64)>),
    RemoveClip(usize, usize),
    MoveClip(usize, usize, i64),
    Tempo(Vec<(i64, f64)>),
    TimeSig(u8, u8),
    Loop(Option<(i64, i64)>),
    AddMarker(i64, String),
    RemoveMarker(usize),
}

fn op() -> impl Strategy<Value = Op> {
    let i = 0usize..8;
    prop_oneof![
        (i.clone(), 0u8..4).prop_map(|(a, k)| Op::AddTrack(a, k)),
        i.clone().prop_map(Op::RemoveTrack),
        (i.clone(), i.clone()).prop_map(|(a, b)| Op::MoveTrack(a, b)),
        (i.clone(), "[a-zA-Z ]{0,12}").prop_map(|(a, n)| Op::Rename(a, n)),
        (i.clone(), any::<[u8; 3]>()).prop_map(|(a, c)| Op::Color(a, c)),
        (
            proptest::option::of(i.clone()),
            -60.0f32..6.0,
            -1.0f32..1.0,
            any::<bool>(),
            any::<bool>()
        )
            .prop_map(|(t, g, p, m, s)| Op::Strip(t, g, p, m, s)),
        (i.clone(), 0i64..100_000, 1i64..50_000).prop_map(|(t, s, l)| Op::AddAudioClip(t, s, l)),
        (
            i.clone(),
            0i64..100_000,
            prop::collection::vec((0u8..128, 1u8..128, 0i64..3840, 1i64..960), 0..6)
        )
            .prop_map(|(t, s, n)| Op::AddMidiClip(t, s, n)),
        (i.clone(), i.clone()).prop_map(|(t, c)| Op::RemoveClip(t, c)),
        (i.clone(), i.clone(), 0i64..100_000).prop_map(|(t, c, s)| Op::MoveClip(t, c, s)),
        prop::collection::vec((0i64..50_000, 20.0f64..300.0), 0..4).prop_map(Op::Tempo),
        (1u8..16, 0u8..5).prop_map(|(n, d)| Op::TimeSig(n, 1 << d)),
        proptest::option::of((0i64..10_000, 1i64..10_000)).prop_map(Op::Loop),
        (0i64..100_000, "[a-z]{1,8}").prop_map(|(t, n)| Op::AddMarker(t, n)),
        i.prop_map(Op::RemoveMarker),
    ]
}

fn track_id(p: &Project, i: usize) -> Option<TrackId> {
    (!p.tracks.is_empty()).then(|| p.tracks[i % p.tracks.len()].id)
}

fn clip_id(p: &Project, t: TrackId, c: usize) -> Option<ClipId> {
    let clips = &p.track(t)?.clips;
    (!clips.is_empty()).then(|| clips[c % clips.len()].id())
}

/// Builds an edit for `op`, or `None` if it doesn't apply (e.g. no tracks yet).
fn to_edit(p: &mut Project, op: Op) -> Option<Edit> {
    Some(match op {
        Op::AddTrack(i, k) => {
            let id = TrackId(p.alloc_id());
            let kind = [
                TrackKind::Audio,
                TrackKind::Instrument,
                TrackKind::Bus,
                TrackKind::Folder,
            ][k as usize % 4];
            Edit::AddTrack {
                index: i % (p.tracks.len() + 1),
                track: Some(Track {
                    id,
                    name: format!("Track {}", id.0),
                    kind,
                    ..Track::default()
                }),
            }
        }
        Op::RemoveTrack(i) => Edit::RemoveTrack {
            id: track_id(p, i)?,
            removed: None,
        },
        Op::MoveTrack(a, b) => {
            let n = p.tracks.len();
            (n > 0).then_some(())?;
            Edit::MoveTrack {
                from: a % n,
                to: b % n,
            }
        }
        Op::Rename(i, name) => Edit::RenameTrack {
            id: track_id(p, i)?,
            name,
        },
        Op::Color(i, color) => Edit::SetTrackColor {
            id: track_id(p, i)?,
            color,
        },
        Op::Strip(t, gain_db, pan, mute, solo) => Edit::SetStrip {
            track: match t {
                Some(i) => Some(track_id(p, i)?),
                None => None,
            },
            strip: ChannelStrip {
                gain_db,
                pan,
                mute,
                solo,
            },
        },
        Op::AddAudioClip(t, start, length) => {
            let track = track_id(p, t)?;
            Edit::AddClip {
                track,
                clip: Some(Clip::Audio(AudioClip {
                    id: ClipId(p.alloc_id()),
                    asset: AssetId(1),
                    start: Tick(start),
                    length: Tick(length),
                    source_offset: SamplePos(start as u64),
                    gain_db: -3.5,
                    fade_in: Tick(10),
                    fade_out: Tick(20),
                })),
            }
        }
        Op::AddMidiClip(t, start, notes) => {
            let track = track_id(p, t)?;
            Edit::AddClip {
                track,
                clip: Some(Clip::Midi(MidiClip {
                    id: ClipId(p.alloc_id()),
                    start: Tick(start),
                    length: Tick(3840),
                    notes: notes
                        .into_iter()
                        .map(|(pitch, velocity, s, l)| Note {
                            pitch,
                            velocity,
                            start: Tick(s),
                            length: Tick(l),
                            channel: 0,
                        })
                        .collect(),
                })),
            }
        }
        Op::RemoveClip(t, c) => {
            let track = track_id(p, t)?;
            Edit::RemoveClip {
                track,
                clip: clip_id(p, track, c)?,
                removed: None,
            }
        }
        Op::MoveClip(t, c, start) => {
            let track = track_id(p, t)?;
            Edit::MoveClip {
                track,
                clip: clip_id(p, track, c)?,
                start: Tick(start),
            }
        }
        Op::Tempo(points) => Edit::SetTempo(TempoMap::from_points(
            points
                .into_iter()
                .map(|(t, bpm)| TempoPoint { tick: Tick(t), bpm })
                .collect(),
        )),
        Op::TimeSig(n, d) => Edit::SetTimeSig(TimeSig::new(n, d)),
        Op::Loop(r) => Edit::SetLoop(r.map(|(a, l)| (Tick(a), Tick(a + l)))),
        Op::AddMarker(t, name) => Edit::AddMarker(Some(Marker {
            id: MarkerId(p.alloc_id()),
            tick: Tick(t),
            name,
        })),
        Op::RemoveMarker(i) => {
            let n = p.markers.len();
            (n > 0).then_some(())?;
            Edit::RemoveMarker {
                id: p.markers[i % n].id,
                removed: None,
            }
        }
    })
}

/// The id counter is not undone on purpose (ids are never reused); ignore it.
fn same_content(a: &Project, b: &Project) -> bool {
    let mut b = b.clone();
    b.next_id = a.next_id;
    *a == b
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("musiton-proj-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn hundred_random_edits_then_undo_all_restores_original(
        ops in prop::collection::vec(op(), 100),
        merge in prop::collection::vec(any::<bool>(), 100),
    ) {
        let mut p = Project::default();
        let original = p.clone();
        let mut undo = UndoStack::new();
        let mut steps = 0;
        for (op, m) in ops.into_iter().zip(merge) {
            if let Some(edit) = to_edit(&mut p, op) {
                undo.perform(&mut p, edit, m).unwrap();
                steps += 1;
            }
        }
        let edited = p.clone();
        prop_assert!(steps > 0);

        // Save/load round trip of the edited project.
        let dir = temp_dir(&format!("rt{}", edited.next_id));
        let path = dir.join("song.musiton");
        save(&edited, &path).unwrap();
        prop_assert_eq!(&load(&path).unwrap(), &edited);
        let _ = std::fs::remove_dir_all(&dir);

        while undo.undo(&mut p).unwrap().is_some() {}
        prop_assert!(same_content(&original, &p));
        while undo.redo(&mut p).unwrap().is_some() {}
        prop_assert!(same_content(&edited, &p));
    }
}

#[test]
fn dirty_flag_follows_save_point() {
    let mut p = Project::default();
    let mut u = UndoStack::new();
    assert!(!u.is_dirty());
    u.perform(&mut p, Edit::SetTimeSig(TimeSig::new(3, 4)), false)
        .unwrap();
    assert!(u.is_dirty());
    u.mark_saved();
    assert!(!u.is_dirty());
    u.undo(&mut p).unwrap();
    assert!(u.is_dirty());
    u.redo(&mut p).unwrap();
    assert!(!u.is_dirty());
    // A merged change after saving makes the project dirty again.
    u.perform(&mut p, Edit::SetTimeSig(TimeSig::new(5, 4)), true)
        .unwrap();
    assert!(u.is_dirty());
    assert_eq!(u.undo_label(), Some("Time signature"));
    u.undo(&mut p).unwrap();
    assert_eq!(p.time_sig, TimeSig::default());
}

#[test]
fn merged_drag_is_one_undo_step() {
    let mut p = Project::default();
    let mut u = UndoStack::new();
    for bpm in [121.0, 125.0, 130.0] {
        u.perform(&mut p, Edit::SetTempo(TempoMap::constant(bpm)), true)
            .unwrap();
    }
    assert_eq!(p.tempo, TempoMap::constant(130.0));
    u.undo(&mut p).unwrap();
    assert_eq!(p.tempo, TempoMap::default());
    assert!(u.undo(&mut p).unwrap().is_none());
}

#[test]
fn failed_edit_changes_nothing() {
    let mut p = Project::default();
    let mut u = UndoStack::new();
    let before = p.clone();
    let err = u.perform(
        &mut p,
        Edit::RenameTrack {
            id: TrackId(42),
            name: "x".into(),
        },
        false,
    );
    assert!(err.is_err());
    assert_eq!(p, before);
    assert!(!u.is_dirty());
}

#[test]
fn file_layout_and_version_checks() {
    let dir = temp_dir("layout");
    let chosen = dir.join("MySong.musiton");
    let path = project_dir_for(&chosen);
    assert_eq!(path, dir.join("MySong").join("MySong.musiton"));
    // Choosing inside a folder that already has the song's name doesn't nest again.
    assert_eq!(project_dir_for(&path), path);

    save(&Project::default(), &path).unwrap();
    for sub in ["audio", "peaks", "plugins", "autosave"] {
        assert!(path.parent().unwrap().join(sub).is_dir());
    }
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("version: 1"));

    std::fs::write(&path, text.replace("version: 1", "version: 99")).unwrap();
    assert!(matches!(load(&path), Err(ProjectError::TooNew(99))));
    std::fs::write(&path, "Project(tracks: [oops").unwrap();
    assert!(matches!(load(&path), Err(ProjectError::Parse(_))));
    // Unknown fields from newer minor additions and missing fields are tolerated.
    std::fs::write(&path, "(version: 1, metronome: false, future_thing: 3)").unwrap();
    let p = load(&path).unwrap();
    assert!(!p.metronome);
    assert_eq!(p.sample_rate, 48_000);
    let _ = std::fs::remove_dir_all(&dir);
}
