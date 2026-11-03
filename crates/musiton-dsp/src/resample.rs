//! Pitch shifting of samples: cubic interpolation plus pre-filtered octave levels
//! ("mipmaps") so that playing a sample higher does not alias.

/// Number of extra octave levels kept (up to +4 octaves without aliasing).
pub const OCTAVE_LEVELS: usize = 4;

/// Low-pass for halving the sample rate (Blackman-windowed sinc, 63 taps, cutoff at
/// 0.45 × the new Nyquist so the stop band starts before it).
fn halfband_taps() -> Vec<f32> {
    let n = 63;
    let mid = (n / 2) as f32;
    let fc = 0.225_f32;
    (0..n)
        .map(|i| {
            let x = i as f32 - mid;
            let sinc = if x == 0.0 {
                2.0 * fc
            } else {
                (std::f32::consts::TAU * fc * x).sin() / (std::f32::consts::PI * x)
            };
            let w = 0.42 - 0.5 * (std::f32::consts::TAU * i as f32 / (n - 1) as f32).cos()
                + 0.08 * (2.0 * std::f32::consts::TAU * i as f32 / (n - 1) as f32).cos();
            sinc * w
        })
        .collect()
}

/// Filters and decimates by 2.
pub fn downsample2(x: &[f32]) -> Vec<f32> {
    let taps = halfband_taps();
    let half = taps.len() / 2;
    (0..x.len().div_ceil(2))
        .map(|j| {
            let c = 2 * j;
            taps.iter()
                .enumerate()
                .map(|(k, t)| {
                    let i = c as isize + k as isize - half as isize;
                    if i < 0 {
                        0.0
                    } else {
                        x.get(i as usize).copied().unwrap_or(0.0) * t
                    }
                })
                .sum()
        })
        .collect()
}

/// A channel with its octave-down levels. Built off the audio thread.
#[derive(Debug, Clone, Default)]
pub struct Mipmap {
    levels: Vec<Vec<f32>>,
}

impl Mipmap {
    /// Builds the levels for `x`.
    pub fn new(x: &[f32]) -> Self {
        let mut levels = vec![x.to_vec()];
        for _ in 0..OCTAVE_LEVELS {
            let next = levels.last().map(|l| downsample2(l)).unwrap_or_default();
            levels.push(next);
        }
        Self { levels }
    }

    /// Length of the original.
    pub fn len(&self) -> usize {
        self.levels.first().map_or(0, Vec::len)
    }

    /// `true` if empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Sample at position `pos` (in original samples) when playing `ratio` times faster.
    #[inline]
    pub fn read(&self, pos: f64, ratio: f64) -> f32 {
        let level = if ratio > 1.0 {
            (ratio.log2().floor() as usize).min(OCTAVE_LEVELS)
        } else {
            0
        };
        let Some(data) = self.levels.get(level) else {
            return 0.0;
        };
        cubic(data, pos / f64::from(1u32 << level))
    }
}

/// Cubic (Catmull-Rom) interpolation of `x` at fractional index `pos`.
#[inline]
pub fn cubic(x: &[f32], pos: f64) -> f32 {
    if pos < 0.0 {
        return 0.0;
    }
    let i = pos.floor() as usize;
    let t = (pos - pos.floor()) as f32;
    let at = |k: isize| {
        let j = i as isize + k;
        if j < 0 {
            0.0
        } else {
            x.get(j as usize).copied().unwrap_or(0.0)
        }
    };
    let (y0, y1, y2, y3) = (at(-1), at(0), at(1), at(2));
    let a = -0.5 * y0 + 1.5 * y1 - 1.5 * y2 + 0.5 * y3;
    let b = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c = -0.5 * y0 + 0.5 * y2;
    ((a * t + b) * t + c) * t + y1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{db, magnitude};

    fn sine(hz: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (std::f32::consts::TAU * hz * i as f32 / 48_000.0).sin())
            .collect()
    }

    fn play(m: &Mipmap, ratio: f64, n: usize) -> Vec<f32> {
        (0..n).map(|i| m.read(i as f64 * ratio, ratio)).collect()
    }

    #[test]
    fn octave_up_keeps_pitch_and_level() {
        let m = Mipmap::new(&sine(1000.0, 48_000));
        let y = play(&m, 2.0, 9_600);
        let x = sine(2000.0, 9_600);
        assert!(
            db(
                magnitude(&y, 2000.0, 48_000.0),
                magnitude(&x, 2000.0, 48_000.0)
            )
            .abs()
                < 0.5
        );
    }

    #[test]
    fn content_above_nyquist_after_pitching_is_removed() {
        // 15 kHz played an octave up would be 30 kHz → aliases to 18 kHz without filtering.
        let m = Mipmap::new(&sine(15_000.0, 48_000));
        let y = play(&m, 2.0, 9_600);
        let reference = magnitude(&sine(15_000.0, 9_600), 15_000.0, 48_000.0);
        assert!(db(magnitude(&y, 18_000.0, 48_000.0), reference) < -40.0);
    }

    #[test]
    fn cubic_hits_samples_exactly() {
        let x = [0.0, 1.0, 4.0, 9.0, 16.0];
        assert_eq!(cubic(&x, 2.0), 4.0);
        assert!((cubic(&x, 2.5) - 6.25).abs() < 0.2);
    }
}
