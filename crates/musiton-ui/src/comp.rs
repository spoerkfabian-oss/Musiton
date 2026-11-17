//! Comping: choosing, range by range, which recorded take of an audio track plays.
//!
//! Takes are the audio clips of a track that overlap in time (loop recording and recording
//! over earlier material create them; the newest one plays, older ones are muted). Clips
//! from the same recording share an audio file, so a take is identified by its asset.
//! Comping a range splits every clip it touches at the range edges and, inside the range,
//! unmutes the chosen take and mutes the others.

use musiton_core::ids::{AssetId, ClipId, TrackId};
use musiton_core::{TempoMap, Tick, PPQ};
use musiton_project::{Clip, Edit, Track, TrackKind};

use crate::clip_edit;

/// Short fades at comp edges so switching takes does not click (≈ 15 ms at 120 BPM).
pub const COMP_FADE: i64 = PPQ / 32;

/// The takes of an audio track: assets of clips that overlap a clip of another asset,
/// oldest recording (lowest asset id) first. Empty when the track has no overlaps.
pub fn takes(track: &Track) -> Vec<AssetId> {
    if track.kind != TrackKind::Audio {
        return Vec::new();
    }
    let clips: Vec<(AssetId, i64, i64)> = track
        .clips
        .iter()
        .filter_map(|c| match c {
            Clip::Audio(a) => Some((a.asset, a.start.0, a.start.0 + a.length.0)),
            Clip::Midi(_) => None,
        })
        .collect();
    let mut out: Vec<AssetId> = Vec::new();
    for (i, a) in clips.iter().enumerate() {
        let overlaps = clips
            .iter()
            .enumerate()
            .any(|(j, b)| i != j && a.0 != b.0 && a.1 < b.2 && b.1 < a.2);
        if overlaps && !out.contains(&a.0) {
            out.push(a.0);
        }
    }
    out.sort_by_key(|a| a.0);
    out
}

/// The track's clips after choosing take `take` for `t0..t1`. New pieces get ids from
/// `next_id`. Returns `None` when nothing changes.
pub fn comp(
    clips: &[Clip],
    take: AssetId,
    (t0, t1): (Tick, Tick),
    tempo: &TempoMap,
    sample_rate: f64,
    next_id: &mut dyn FnMut() -> ClipId,
) -> Option<Vec<Clip>> {
    let (t0, t1) = (t0.min(t1), t0.max(t1));
    if t1.0 <= t0.0 {
        return None;
    }
    let mut out = Vec::with_capacity(clips.len() + 4);
    for c in clips {
        let Clip::Audio(a) = c else {
            out.push(c.clone());
            continue;
        };
        let (start, end) = (a.start.0, a.start.0 + a.length.0);
        if end <= t0.0 || start >= t1.0 {
            out.push(c.clone());
            continue;
        }
        // Cut at the range edges that fall inside the clip.
        let mut middle = c.clone();
        if t0.0 > start {
            if let Some((left, right)) =
                clip_edit::split(&middle, t0, next_id(), tempo, sample_rate)
            {
                out.push(with_fades(left, None, Some(COMP_FADE)));
                middle = right;
            }
        }
        let mut after = None;
        if t1.0 < end {
            if let Some((left, right)) =
                clip_edit::split(&middle, t1, next_id(), tempo, sample_rate)
            {
                middle = left;
                after = Some(with_fades(right, Some(COMP_FADE), None));
            }
        }
        if let Clip::Audio(m) = &mut middle {
            m.muted = m.asset != take;
        }
        let fade_in = (t0.0 > start).then_some(COMP_FADE);
        let fade_out = (t1.0 < end).then_some(COMP_FADE);
        out.push(with_fades(middle, fade_in, fade_out));
        out.extend(after);
    }
    (out != clips).then_some(out)
}

fn with_fades(mut clip: Clip, fade_in: Option<i64>, fade_out: Option<i64>) -> Clip {
    if let Clip::Audio(a) = &mut clip {
        let half = a.length.0 / 2;
        if let Some(f) = fade_in {
            a.fade_in = Tick(f.min(half));
        }
        if let Some(f) = fade_out {
            a.fade_out = Tick(f.min(half));
        }
    }
    clip
}

/// The edits that turn `track`'s clips into `new`: removals, replacements, additions,
/// as one undo step.
pub fn edits(track: &Track, new: &[Clip]) -> Edit {
    let mut edits = Vec::new();
    for old in &track.clips {
        if !new.iter().any(|c| c.id() == old.id()) {
            edits.push(Edit::RemoveClip {
                track: track.id,
                clip: old.id(),
                removed: None,
            });
        }
    }
    for c in new {
        match track.clips.iter().find(|o| o.id() == c.id()) {
            Some(old) if old == c => {}
            Some(_) => edits.push(Edit::ReplaceClip {
                track: track.id,
                clip: c.clone(),
            }),
            None => edits.push(Edit::AddClip {
                track: track.id,
                clip: Some(c.clone()),
            }),
        }
    }
    Edit::Batch(edits)
}

/// Which take plays at `tick` on `track` (the unmuted clip there), if any.
pub fn active_take(track: &Track, tick: i64) -> Option<AssetId> {
    track.clips.iter().find_map(|c| match c {
        Clip::Audio(a) if !a.muted && (a.start.0..a.start.0 + a.length.0).contains(&tick) => {
            Some(a.asset)
        }
        _ => None,
    })
}

/// A track id with its takes, for the arrangement's take rows.
pub type TrackTakes = (TrackId, Vec<AssetId>);

#[cfg(test)]
mod tests {
    use super::*;
    use musiton_core::SamplePos;
    use musiton_project::{AudioClip, UndoStack};

    fn take(id: u64, asset: u64, start: i64, len: i64, muted: bool) -> Clip {
        Clip::Audio(AudioClip {
            id: ClipId(id),
            asset: AssetId(asset),
            start: Tick(start),
            length: Tick(len),
            source_offset: SamplePos(0),
            muted,
            ..AudioClip::default()
        })
    }

    fn playing(track: &Track, tick: i64) -> Option<u64> {
        active_take(track, tick).map(|a| a.0)
    }

    #[test]
    fn comping_a_range_switches_takes_there_only_and_undoes_in_one_step() {
        let bar = 4 * PPQ;
        // Two passes over the same 4 bars: take 2 (newer) plays, take 1 is muted.
        let mut p = musiton_project::Project::default();
        p.tracks.push(Track {
            id: TrackId(1),
            clips: vec![
                take(10, 1, 0, 4 * bar, true),
                take(11, 2, 0, 4 * bar, false),
            ],
            ..Track::default()
        });
        let original = p.clone();
        assert_eq!(takes(&p.tracks[0]), [AssetId(1), AssetId(2)]);

        // Use take 1 for bar 2.
        let mut next = 100;
        let mut ids = || {
            next += 1;
            ClipId(next)
        };
        let new = comp(
            &p.tracks[0].clips,
            AssetId(1),
            (Tick(bar), Tick(2 * bar)),
            &p.tempo,
            48_000.0,
            &mut ids,
        )
        .unwrap();
        let edit = edits(&p.tracks[0], &new);
        let mut undo = UndoStack::default();
        undo.perform(&mut p, edit, false).unwrap();
        let t = &p.tracks[0];
        assert_eq!(playing(t, bar / 2), Some(2));
        assert_eq!(playing(t, bar + bar / 2), Some(1));
        assert_eq!(playing(t, 3 * bar), Some(2));
        // Exactly one take plays everywhere, and the cuts are faded.
        for tick in (0..4 * bar).step_by(120) {
            let n = t
                .clips
                .iter()
                .filter(|c| match c {
                    Clip::Audio(a) => {
                        !a.muted && (a.start.0..a.start.0 + a.length.0).contains(&tick)
                    }
                    Clip::Midi(_) => false,
                })
                .count();
            assert_eq!(n, 1, "at tick {tick}");
        }
        let piece = t
            .clips
            .iter()
            .find_map(|c| match c {
                Clip::Audio(a) if a.asset == AssetId(1) && a.start == Tick(bar) => Some(a),
                _ => None,
            })
            .unwrap();
        assert_eq!((piece.fade_in.0, piece.fade_out.0), (COMP_FADE, COMP_FADE));
        // Right part of a split clip continues the audio where the left part stopped.
        assert_eq!(piece.source_offset, SamplePos(96_000));

        // Choosing the take that already plays changes nothing.
        let again = comp(
            &p.tracks[0].clips,
            AssetId(1),
            (Tick(bar), Tick(2 * bar)),
            &p.tempo,
            48_000.0,
            &mut ids,
        );
        assert!(again.is_none());

        undo.undo(&mut p).unwrap();
        assert_eq!(p, original);
    }

    #[test]
    fn tracks_without_overlaps_have_no_takes() {
        let t = Track {
            clips: vec![take(1, 1, 0, 960, false), take(2, 2, 960, 960, false)],
            ..Track::default()
        };
        assert!(takes(&t).is_empty());
    }
}
