//! Time-stretching without changing pitch: WSOLA (waveform-similarity overlap-add).
//!
//! The output is built from overlapping, windowed grains of the input. Each grain is taken
//! near the source position that the time map asks for, shifted by up to `TOLERANCE`
//! frames so that it lines up with the waveform of the previous grain (cross-correlation).
//! That keeps the pitch and avoids the phasing of plain overlap-add.
//!
//! Runs offline (allocates); not for the audio thread.

/// Grain length in frames (≈ 43 ms at 48 kHz).
pub const GRAIN: usize = 2048;
/// Distance between grains in the output (4× overlap).
pub const HOP: usize = GRAIN / 4;
/// How far a grain may move to match the previous one.
pub const TOLERANCE: usize = 384;
/// Only every n-th frame is used when comparing waveforms (speed).
const CORRELATION_STEP: usize = 4;

/// Stretches planar `input` to `out_len` frames. `source_at(n)` is the input position
/// (fractional frames) that output frame `n` should play; it should rise monotonically.
pub fn wsola(
    input: &[Vec<f32>],
    out_len: usize,
    source_at: &dyn Fn(usize) -> f64,
) -> Vec<Vec<f32>> {
    let channels = input.len().max(1);
    let in_len = input.first().map_or(0, Vec::len);
    let mut out = vec![vec![0.0_f32; out_len]; channels];
    if in_len == 0 || out_len == 0 {
        return out;
    }
    let window: Vec<f32> = (0..GRAIN)
        .map(|i| {
            let x = i as f32 / GRAIN as f32;
            0.5 - 0.5 * (std::f32::consts::TAU * x).cos()
        })
        .collect();
    let mut norm = vec![0.0_f32; out_len];
    // Mono mix used for the similarity search.
    let mono: Vec<f32> = (0..in_len)
        .map(|i| {
            input
                .iter()
                .map(|c| c.get(i).copied().unwrap_or(0.0))
                .sum::<f32>()
        })
        .collect();
    let at = |i: isize| -> f32 {
        if i < 0 {
            0.0
        } else {
            mono.get(i as usize).copied().unwrap_or(0.0)
        }
    };

    // Where the previous grain's natural continuation would start in the input.
    let mut follow: Option<isize> = None;
    // The first grain is centred on output frame 0, so the very start maps exactly too.
    let half = (GRAIN / 2) as isize;
    let mut start = -half;
    while start < out_len as isize {
        let centre = (start + half).max(0) as usize;
        let wanted = (source_at(centre) - half as f64).round() as isize;
        let pos = match follow {
            None => wanted,
            Some(target) => {
                // Best shift: grain at `wanted + d` most similar to the continuation.
                let tol = TOLERANCE as isize;
                let mut best = (f32::MIN, 0isize);
                for d in -tol..=tol {
                    let cand = wanted + d;
                    let (mut sum, mut energy) = (0.0_f32, 1e-9_f32);
                    let mut i = 0;
                    while i < GRAIN / 2 {
                        let k = i as isize;
                        let c = at(cand + k);
                        sum += c * at(target + k);
                        energy += c * c;
                        i += CORRELATION_STEP;
                    }
                    // Normalized, so the exact continuation (when there is one) wins.
                    let score = sum / energy.sqrt();
                    if score > best.0 {
                        best = (score, d);
                    }
                }
                wanted + best.1
            }
        };
        for (ch, dst) in out.iter_mut().enumerate() {
            let src = input.get(ch).or_else(|| input.first());
            let Some(src) = src else {
                continue;
            };
            for (i, w) in window.iter().enumerate() {
                let o = start + i as isize;
                if o < 0 {
                    continue;
                }
                let o = o as usize;
                if o >= out_len {
                    break;
                }
                let s = pos + i as isize;
                let v = if s < 0 {
                    0.0
                } else {
                    src.get(s as usize).copied().unwrap_or(0.0)
                };
                if let Some(d) = dst.get_mut(o) {
                    *d += v * w;
                }
            }
        }
        for (i, w) in window.iter().enumerate() {
            let o = start + i as isize;
            if o < 0 {
                continue;
            }
            if let Some(n) = norm.get_mut(o as usize) {
                *n += w;
            }
        }
        follow = Some(pos + HOP as isize);
        start += HOP as isize;
    }
    for ch in &mut out {
        for (s, n) in ch.iter_mut().zip(&norm) {
            if *n > 1e-3 {
                *s /= n;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| (std::f32::consts::TAU * hz * i as f32 / 48_000.0).sin() * 0.5)
            .collect()
    }

    fn crossings(x: &[f32]) -> usize {
        x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn stretching_keeps_the_pitch_and_changes_the_length() {
        let input = vec![sine(440.0, 48_000)];
        // Twice as long: output frame n plays input frame n / 2.
        let out = wsola(&input, 96_000, &|n| n as f64 / 2.0);
        assert_eq!(out[0].len(), 96_000);
        let mid = &out[0][4_800..91_200];
        // Still 440 Hz: 440 crossings per second over 1.8 s.
        let per_second = crossings(mid) as f32 / 1.8;
        assert!((per_second - 440.0).abs() < 4.0, "{per_second} Hz");
        // No level dips between grains.
        for chunk in mid.chunks(2_400) {
            let r = rms(chunk);
            assert!((r - 0.354).abs() < 0.03, "rms {r}");
        }

        // Half as long.
        let out = wsola(&input, 24_000, &|n| n as f64 * 2.0);
        let mid = &out[0][2_400..21_600];
        let per_second = crossings(mid) as f32 / 0.4;
        assert!((per_second - 440.0).abs() < 8.0, "{per_second} Hz");
    }

    #[test]
    fn identity_map_reproduces_the_input() {
        let input = vec![sine(220.0, 24_000), sine(330.0, 24_000)];
        let out = wsola(&input, 24_000, &|n| n as f64);
        for ch in 0..2 {
            let worst = out[ch][GRAIN..24_000 - GRAIN]
                .iter()
                .zip(&input[ch][GRAIN..24_000 - GRAIN])
                .fold(0.0_f32, |m, (a, b)| m.max((a - b).abs()));
            assert!(worst < 1e-3, "channel {ch}: {worst}");
        }
    }

    #[test]
    fn empty_input_gives_silence() {
        let out = wsola(&[Vec::new()], 100, &|n| n as f64);
        assert_eq!(out, vec![vec![0.0; 100]]);
    }
}
