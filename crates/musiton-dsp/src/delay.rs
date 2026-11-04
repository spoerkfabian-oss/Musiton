//! Delay line with fractional (interpolated) reads.

/// A circular buffer delay line. Allocate with [`DelayLine::new`] off the audio thread.
#[derive(Debug, Clone, Default)]
pub struct DelayLine {
    buf: Vec<f32>,
    write: usize,
}

impl DelayLine {
    /// A line holding up to `max_samples` of delay.
    pub fn new(max_samples: usize) -> Self {
        Self {
            buf: vec![0.0; max_samples.max(2) + 2],
            write: 0,
        }
    }

    /// Writes the next sample.
    #[inline]
    pub fn push(&mut self, x: f32) {
        if let Some(s) = self.buf.get_mut(self.write) {
            *s = x;
        }
        self.write = (self.write + 1) % self.buf.len().max(1);
    }

    /// Reads `delay` samples back (fractional, linear interpolation; ≥ 1).
    #[inline]
    pub fn read(&self, delay: f32) -> f32 {
        let len = self.buf.len();
        let d = delay.clamp(1.0, (len - 2) as f32);
        let pos = self.write as f32 - d;
        let pos = if pos < 0.0 { pos + len as f32 } else { pos };
        let i = pos.floor() as usize % len;
        let frac = pos - pos.floor();
        let a = self.buf.get(i).copied().unwrap_or(0.0);
        let b = self.buf.get((i + 1) % len).copied().unwrap_or(0.0);
        a + (b - a) * frac
    }

    /// Clears the content.
    pub fn clear(&mut self) {
        self.buf.fill(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_by_whole_and_fractional_samples() {
        let mut d = DelayLine::new(100);
        for i in 0..50 {
            d.push(i as f32);
        }
        assert_eq!(d.read(1.0), 49.0);
        assert_eq!(d.read(10.0), 40.0);
        assert!((d.read(10.5) - 39.5).abs() < 1e-5);
        for i in 50..300 {
            d.push(i as f32);
        }
        assert_eq!(d.read(100.0), 200.0);
    }
}
