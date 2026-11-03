//! Pure note operations used by the piano roll.

use musiton_core::{Tick, TimeSig};
use musiton_project::Note;

use crate::snap::Snap;

/// Shortest note the editor creates or resizes to.
pub const MIN_NOTE: i64 = 10;

/// Moves note starts towards the grid by `strength` (0 = unchanged, 1 = fully on grid).
pub fn quantize(notes: &mut [Note], idx: &[usize], snap: Snap, sig: TimeSig, strength: f32) {
    let strength = f64::from(strength.clamp(0.0, 1.0));
    for &i in idx {
        if let Some(n) = notes.get_mut(i) {
            let target = snap.round(n.start.0 as f64, sig).0 as f64;
            let s = n.start.0 as f64;
            n.start = Tick((s + (target - s) * strength).round() as i64);
        }
    }
}

/// Transposes notes by `semitones` (clamped to 0..=127).
pub fn transpose(notes: &mut [Note], idx: &[usize], semitones: i32) {
    for &i in idx {
        if let Some(n) = notes.get_mut(i) {
            n.pitch = (i32::from(n.pitch) + semitones).clamp(0, 127) as u8;
        }
    }
}

/// Appends copies of the selected notes right after the selection; returns their indices.
pub fn duplicate(notes: &mut Vec<Note>, idx: &[usize]) -> Vec<usize> {
    let sel: Vec<Note> = idx.iter().filter_map(|&i| notes.get(i).copied()).collect();
    let (Some(first), Some(last)) = (
        sel.iter().map(|n| n.start.0).min(),
        sel.iter().map(|n| n.start.0 + n.length.0).max(),
    ) else {
        return Vec::new();
    };
    let span = last - first;
    let base = notes.len();
    notes.extend(sel.into_iter().map(|mut n| {
        n.start = Tick(n.start.0 + span);
        n
    }));
    (base..notes.len()).collect()
}

/// Moves notes by `dt` ticks (the first selected note's start snaps to the grid) and
/// `dp` semitones.
pub fn moved(
    orig: &[Note],
    idx: &[usize],
    dt: f64,
    dp: i32,
    snap: Snap,
    sig: TimeSig,
) -> Vec<Note> {
    let mut out = orig.to_vec();
    let anchor = idx
        .iter()
        .filter_map(|&i| orig.get(i))
        .map(|n| n.start.0)
        .min()
        .unwrap_or(0);
    let shift = snap.round(anchor as f64 + dt, sig).0.max(0) - anchor;
    for &i in idx {
        if let Some(n) = out.get_mut(i) {
            n.start = Tick(n.start.0 + shift);
            n.pitch = (i32::from(n.pitch) + dp).clamp(0, 127) as u8;
        }
    }
    out
}

/// Changes note lengths by `dt` ticks; the end of the grabbed note snaps to the grid.
pub fn resized(
    orig: &[Note],
    idx: &[usize],
    grabbed: usize,
    dt: f64,
    snap: Snap,
    sig: TimeSig,
) -> Vec<Note> {
    let mut out = orig.to_vec();
    let Some(g) = orig.get(grabbed) else {
        return out;
    };
    let end = g.start.0 + g.length.0;
    let change = snap.round(end as f64 + dt, sig).0 - end;
    for &i in idx {
        if let Some(n) = out.get_mut(i) {
            n.length = Tick((n.length.0 + change).max(MIN_NOTE));
        }
    }
    out
}

/// Indices of notes overlapping the tick range `t0..t1` and pitch range `p0..=p1`.
pub fn in_rect(notes: &[Note], t0: i64, t1: i64, p0: u8, p1: u8) -> Vec<usize> {
    notes
        .iter()
        .enumerate()
        .filter(|(_, n)| {
            n.start.0 < t1 && n.start.0 + n.length.0 > t0 && (p0..=p1).contains(&n.pitch)
        })
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(start: i64, len: i64, pitch: u8) -> Note {
        Note {
            pitch,
            velocity: 100,
            start: Tick(start),
            length: Tick(len),
            channel: 0,
        }
    }

    const Q: Snap = Snap::Note {
        n: 4,
        triplet: false,
    };

    #[test]
    fn quantize_with_strength() {
        let s = TimeSig::default();
        let mut v = vec![n(1000, 100, 60), n(1900, 100, 62)];
        quantize(&mut v, &[0, 1], Q, s, 1.0);
        assert_eq!((v[0].start, v[1].start), (Tick(960), Tick(1920)));
        let mut v = vec![n(1000, 100, 60)];
        quantize(&mut v, &[0], Q, s, 0.5);
        assert_eq!(v[0].start, Tick(980));
    }

    #[test]
    fn transpose_clamps() {
        let mut v = vec![n(0, 1, 120), n(0, 1, 3)];
        transpose(&mut v, &[0, 1], 12);
        assert_eq!((v[0].pitch, v[1].pitch), (127, 15));
        transpose(&mut v, &[1], -40);
        assert_eq!(v[1].pitch, 0);
    }

    #[test]
    fn duplicate_places_copy_after_selection() {
        let mut v = vec![n(0, 480, 60), n(480, 480, 62), n(5000, 10, 70)];
        let new = duplicate(&mut v, &[0, 1]);
        assert_eq!(new, vec![3, 4]);
        assert_eq!((v[3].start, v[4].start), (Tick(960), Tick(1440)));
    }

    #[test]
    fn move_and_resize_snap() {
        let s = TimeSig::default();
        let v = vec![n(960, 480, 60), n(1440, 480, 64)];
        let m = moved(&v, &[0, 1], 500.0, 2, Q, s);
        assert_eq!(
            (m[0].start, m[1].start, m[0].pitch),
            (Tick(1920), Tick(2400), 62)
        );
        let r = resized(&v, &[0, 1], 0, 300.0, Q, s);
        assert_eq!((r[0].length, r[1].length), (Tick(960), Tick(960)));
        let r = resized(&v, &[0], 0, -5000.0, Snap::Off, s);
        assert_eq!(r[0].length, Tick(MIN_NOTE));
    }

    #[test]
    fn rectangle_selection() {
        let v = vec![n(0, 100, 60), n(200, 100, 62), n(200, 100, 80)];
        assert_eq!(in_rect(&v, 50, 250, 55, 65), vec![0, 1]);
    }
}
