//! Pure clip transformations used by the timeline (move, trim, fades, split).

use musiton_core::{SamplePos, TempoMap, Tick, TimeSig};
use musiton_project::Clip;

use crate::snap::Snap;

/// Shortest clip the user can trim to.
pub const MIN_CLIP_TICKS: i64 = 30;

/// Audio frames between two timeline positions.
pub fn frames_between(tempo: &TempoMap, a: Tick, b: Tick, sample_rate: f64) -> i64 {
    ((tempo.tick_to_seconds(b.0 as f64) - tempo.tick_to_seconds(a.0 as f64)) * sample_rate).round()
        as i64
}

/// Timeline position `frames` audio frames after `from`.
pub fn tick_after_frames(tempo: &TempoMap, from: Tick, frames: f64, sample_rate: f64) -> Tick {
    let secs = tempo.tick_to_seconds(from.0 as f64) + frames / sample_rate.max(1.0);
    Tick(tempo.seconds_to_tick(secs).round() as i64)
}

/// Clip moved by `delta` ticks with its start snapped to the grid (never before 0).
pub fn moved(clip: &Clip, delta: f64, snap: Snap, sig: TimeSig) -> Clip {
    let mut c = clip.clone();
    let start = snap.round(clip.start().0 as f64 + delta, sig).0.max(0);
    c.set_start(Tick(start));
    c
}

/// Clip with its left edge moved by `delta` (audio: the file offset moves along, so the
/// audio stays in place on the timeline).
pub fn trimmed_start(
    clip: &Clip,
    delta: f64,
    snap: Snap,
    sig: TimeSig,
    tempo: &TempoMap,
    sample_rate: f64,
) -> Clip {
    let end = clip.end().0;
    let mut start = snap.round(clip.start().0 as f64 + delta, sig).0;
    start = start.clamp(0, end - MIN_CLIP_TICKS);
    let mut c = clip.clone();
    match &mut c {
        Clip::Audio(a) => {
            // Can't start before the beginning of the file.
            let earliest =
                tick_after_frames(tempo, a.start, -(a.source_offset.0 as f64), sample_rate);
            start = start.max(earliest.0);
            let shift = frames_between(tempo, a.start, Tick(start), sample_rate);
            a.source_offset = SamplePos((a.source_offset.0 as i64 + shift).max(0) as u64);
            a.start = Tick(start);
            a.length = Tick(end - start);
            a.fade_in = Tick(a.fade_in.0.min(a.length.0));
            a.fade_out = Tick(a.fade_out.0.min(a.length.0 - a.fade_in.0));
        }
        Clip::Midi(m) => {
            let shift = start - m.start.0;
            for n in &mut m.notes {
                n.start = Tick(n.start.0 - shift);
            }
            m.notes.retain(|n| n.start.0 >= 0);
            m.start = Tick(start);
            m.length = Tick(end - start);
        }
    }
    c
}

/// Clip with its right edge moved by `delta`; audio clips can't extend past the file end
/// (`file_frames`).
pub fn trimmed_end(
    clip: &Clip,
    delta: f64,
    snap: Snap,
    sig: TimeSig,
    tempo: &TempoMap,
    sample_rate: f64,
    file_frames: Option<usize>,
) -> Clip {
    let start = clip.start().0;
    let mut end = snap.round(clip.end().0 as f64 + delta, sig).0;
    end = end.max(start + MIN_CLIP_TICKS);
    let mut c = clip.clone();
    match &mut c {
        Clip::Audio(a) => {
            if let Some(frames) = file_frames {
                let left = frames as f64 - a.source_offset.0 as f64;
                end = end.min(tick_after_frames(tempo, a.start, left, sample_rate).0);
            }
            a.length = Tick((end - start).max(1));
            a.fade_out = Tick(a.fade_out.0.min(a.length.0));
            a.fade_in = Tick(a.fade_in.0.min(a.length.0 - a.fade_out.0));
        }
        Clip::Midi(m) => m.length = Tick(end - start),
    }
    c
}

/// Audio clip with the fade-in ending at timeline position `at` (clamped).
pub fn with_fade_in(clip: &Clip, at: f64) -> Clip {
    let mut c = clip.clone();
    if let Clip::Audio(a) = &mut c {
        let max = a.length.0 - a.fade_out.0;
        a.fade_in = Tick((at.round() as i64 - a.start.0).clamp(0, max.max(0)));
    }
    c
}

/// Audio clip with the fade-out starting at timeline position `at` (clamped).
pub fn with_fade_out(clip: &Clip, at: f64) -> Clip {
    let mut c = clip.clone();
    if let Clip::Audio(a) = &mut c {
        let end = a.start.0 + a.length.0;
        let max = a.length.0 - a.fade_in.0;
        a.fade_out = Tick((end - at.round() as i64).clamp(0, max.max(0)));
    }
    c
}

/// Splits `clip` at `at` into (left, right). `right` gets `right_id`.
/// `None` if `at` is not inside the clip (with room for both halves).
pub fn split(
    clip: &Clip,
    at: Tick,
    right_id: musiton_core::ids::ClipId,
    tempo: &TempoMap,
    sample_rate: f64,
) -> Option<(Clip, Clip)> {
    let (start, end) = (clip.start().0, clip.end().0);
    if at.0 < start + MIN_CLIP_TICKS || at.0 > end - MIN_CLIP_TICKS {
        return None;
    }
    let mut left = clip.clone();
    let mut right = clip.clone();
    right.set_id(right_id);
    match (&mut left, &mut right) {
        (Clip::Audio(l), Clip::Audio(r)) => {
            l.length = Tick(at.0 - start);
            l.fade_out = Tick(0);
            l.fade_in = Tick(l.fade_in.0.min(l.length.0));
            r.start = at;
            r.length = Tick(end - at.0);
            r.source_offset = SamplePos(
                (l.source_offset.0 as i64 + frames_between(tempo, Tick(start), at, sample_rate))
                    .max(0) as u64,
            );
            r.fade_in = Tick(0);
            r.fade_out = Tick(r.fade_out.0.min(r.length.0));
        }
        (Clip::Midi(l), Clip::Midi(r)) => {
            let cut = at.0 - start;
            l.length = Tick(cut);
            l.notes.retain(|n| n.start.0 < cut);
            for n in &mut l.notes {
                n.length = Tick(n.length.0.min(cut - n.start.0));
            }
            r.start = at;
            r.length = Tick(end - at.0);
            r.notes.retain(|n| n.start.0 >= cut);
            for n in &mut r.notes {
                n.start = Tick(n.start.0 - cut);
            }
        }
        _ => return None,
    }
    Some((left, right))
}

#[cfg(test)]
mod tests {
    use super::*;
    use musiton_core::ids::{AssetId, ClipId};
    use musiton_core::PPQ;
    use musiton_project::AudioClip;

    const SR: f64 = 48_000.0;

    fn audio(start: i64, len: i64, offset: u64) -> Clip {
        Clip::Audio(AudioClip {
            id: ClipId(1),
            asset: AssetId(1),
            start: Tick(start),
            length: Tick(len),
            source_offset: SamplePos(offset),
            ..AudioClip::default()
        })
    }

    fn a(c: &Clip) -> &AudioClip {
        match c {
            Clip::Audio(a) => a,
            Clip::Midi(_) => panic!("not audio"),
        }
    }

    #[test]
    fn move_snaps_start() {
        let c = audio(3840, 960, 0);
        let s = TimeSig::default();
        assert_eq!(moved(&c, 1000.0, Snap::Bar, s).start(), Tick(3840));
        assert_eq!(moved(&c, 2500.0, Snap::Bar, s).start(), Tick(7680));
        assert_eq!(moved(&c, -9999.0, Snap::Off, s).start(), Tick(0));
    }

    #[test]
    fn trim_start_keeps_audio_in_place() {
        let tempo = TempoMap::constant(120.0);
        let c = audio(PPQ * 4, PPQ * 4, 0);
        // Moving the left edge one beat right skips one beat (24 000 frames) of audio.
        let t = trimmed_start(&c, PPQ as f64, Snap::Off, TimeSig::default(), &tempo, SR);
        assert_eq!(a(&t).start, Tick(PPQ * 5));
        assert_eq!(a(&t).length, Tick(PPQ * 3));
        assert_eq!(a(&t).source_offset, SamplePos(24_000));
        // Can't extend before the file start.
        let t = trimmed_start(&c, -(PPQ as f64), Snap::Off, TimeSig::default(), &tempo, SR);
        assert_eq!(a(&t).start, Tick(PPQ * 4));
        assert_eq!(a(&t).source_offset, SamplePos(0));
    }

    #[test]
    fn trim_end_stops_at_file_end() {
        let tempo = TempoMap::constant(120.0);
        let c = audio(0, PPQ, 0);
        let t = trimmed_end(
            &c,
            100.0 * PPQ as f64,
            Snap::Off,
            TimeSig::default(),
            &tempo,
            SR,
            Some(48_000),
        );
        assert_eq!(a(&t).length, Tick(2 * PPQ));
        let t = trimmed_end(
            &c,
            -10.0 * PPQ as f64,
            Snap::Off,
            TimeSig::default(),
            &tempo,
            SR,
            None,
        );
        assert_eq!(a(&t).length, Tick(MIN_CLIP_TICKS));
    }

    #[test]
    fn fades_are_clamped() {
        let c = audio(1000, 2000, 0);
        assert_eq!(a(&with_fade_in(&c, 1500.0)).fade_in, Tick(500));
        assert_eq!(a(&with_fade_in(&c, 99_999.0)).fade_in, Tick(2000));
        let both = with_fade_out(&with_fade_in(&c, 2500.0), 2000.0);
        assert_eq!(a(&both).fade_out, Tick(500));
        assert_eq!(a(&with_fade_out(&c, 0.0)).fade_out, Tick(2000));
    }

    #[test]
    fn split_audio_continues_seamlessly() {
        let tempo = TempoMap::constant(120.0);
        let c = audio(0, 4 * PPQ, 1000);
        let (l, r) = split(&c, Tick(PPQ), ClipId(2), &tempo, SR).unwrap();
        assert_eq!(a(&l).length, Tick(PPQ));
        assert_eq!(a(&r).start, Tick(PPQ));
        assert_eq!(a(&r).length, Tick(3 * PPQ));
        assert_eq!(a(&r).source_offset, SamplePos(1000 + 24_000));
        assert_eq!(r.id(), ClipId(2));
        assert!(split(&c, Tick(5 * PPQ), ClipId(3), &tempo, SR).is_none());
    }
}
