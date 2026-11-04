//! Small radix-2 FFT for spectrum displays (not used on the audio thread).

/// Magnitudes (linear) of the first half of the spectrum of `x` (length = power of two),
/// Hann-windowed and normalized so a full-scale sine reads ≈ 1.
pub fn magnitude_spectrum(x: &[f32]) -> Vec<f32> {
    let n = x.len();
    if n < 2 || !n.is_power_of_two() {
        return Vec::new();
    }
    let mut re: Vec<f32> = x
        .iter()
        .enumerate()
        .map(|(i, &s)| s * (0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos()))
        .collect();
    let mut im = vec![0.0_f32; n];
    // Bit reversal.
    let bits = n.trailing_zeros();
    for i in 0..n {
        let j = i.reverse_bits() >> (usize::BITS - bits);
        if j > i {
            re.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -std::f32::consts::TAU / len as f32;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (s, c) = (ang * k as f32).sin_cos();
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * c - im[b] * s;
                let ti = re[b] * s + im[b] * c;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len *= 2;
    }
    (0..n / 2)
        .map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt() * 4.0 / n as f32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_shows_up_in_its_bin() {
        let n = 2048;
        let bin = 100;
        let x: Vec<f32> = (0..n)
            .map(|i| (std::f32::consts::TAU * bin as f32 * i as f32 / n as f32).sin())
            .collect();
        let m = magnitude_spectrum(&x);
        assert_eq!(m.len(), n / 2);
        let peak = m
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0;
        assert_eq!(peak, bin);
        assert!((m[bin] - 1.0).abs() < 0.05, "{}", m[bin]);
        assert!(magnitude_spectrum(&[0.0; 3]).is_empty());
    }
}
