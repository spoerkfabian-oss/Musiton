//! Parameter smoothing (avoids clicks and zipper noise).

/// Linear ramp from the current value to a target over a fixed number of samples.
#[derive(Debug, Clone)]
pub struct LinearRamp {
    current: f32,
    target: f32,
    step: f32,
    remaining: u32,
    ramp_len: u32,
}

impl LinearRamp {
    /// Creates a ramp resting at `value`; changes take `ramp_len` samples (min 1).
    pub fn new(value: f32, ramp_len: u32) -> Self {
        Self {
            current: value,
            target: value,
            step: 0.0,
            remaining: 0,
            ramp_len: ramp_len.max(1),
        }
    }

    /// Creates a ramp whose length is `ms` milliseconds at `sample_rate`.
    pub fn with_time(value: f32, ms: f32, sample_rate: f32) -> Self {
        Self::new(value, (ms * 0.001 * sample_rate).round() as u32)
    }

    /// Starts moving towards `target`. Does nothing if it is already the target.
    pub fn set_target(&mut self, target: f32) {
        if target == self.target {
            return;
        }
        self.target = target;
        self.remaining = self.ramp_len;
        self.step = (target - self.current) / self.ramp_len as f32;
    }

    /// Jumps to `value` immediately.
    pub fn set_immediate(&mut self, value: f32) {
        self.current = value;
        self.target = value;
        self.remaining = 0;
    }

    /// The value the ramp is heading to.
    pub fn target(&self) -> f32 {
        self.target
    }

    /// `true` while the ramp has not reached its target.
    pub fn is_ramping(&self) -> bool {
        self.remaining > 0
    }

    /// Returns the next value.
    #[inline]
    pub fn next_value(&mut self) -> f32 {
        if self.remaining > 0 {
            self.remaining -= 1;
            self.current = if self.remaining == 0 {
                self.target
            } else {
                self.current + self.step
            };
        }
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;

    #[test]
    fn reaches_target_exactly_after_ramp_len() {
        let mut r = LinearRamp::new(0.0, 4);
        r.set_target(1.0);
        let v: Vec<f32> = (0..6).map(|_| r.next_value()).collect();
        assert_abs_diff_eq!(v[0], 0.25);
        assert_abs_diff_eq!(v[1], 0.5);
        assert_abs_diff_eq!(v[2], 0.75);
        assert_eq!(v[3], 1.0);
        assert_eq!(v[5], 1.0);
        assert!(!r.is_ramping());
    }

    #[test]
    fn retarget_mid_ramp_is_continuous() {
        let mut r = LinearRamp::new(0.0, 10);
        r.set_target(1.0);
        let mut last = 0.0;
        for _ in 0..5 {
            last = r.next_value();
        }
        r.set_target(0.0);
        let next = r.next_value();
        assert!((next - last).abs() <= 0.1 + 1e-6);
    }

    #[test]
    fn with_time_computes_length() {
        let mut r = LinearRamp::with_time(0.0, 5.0, 48_000.0);
        r.set_target(1.0);
        let n = std::iter::from_fn(|| r.is_ramping().then(|| r.next_value())).count();
        assert_eq!(n, 240);
    }
}

/// One-pole smoother: follows its target exponentially (~99 % after `ms`).
#[derive(Debug, Clone, Copy)]
pub struct OnePole {
    value: f32,
    target: f32,
    coef: f32,
}

impl Default for OnePole {
    fn default() -> Self {
        Self {
            value: 0.0,
            target: 0.0,
            coef: 1.0,
        }
    }
}

impl OnePole {
    /// Smoother at `value` that settles in about `ms` milliseconds.
    pub fn new(value: f32, ms: f32, sample_rate: f32) -> Self {
        Self {
            value,
            target: value,
            coef: 1.0 - (-4.6 / (ms.max(0.01) * 0.001 * sample_rate.max(1.0))).exp(),
        }
    }

    /// Sets the target.
    pub fn set(&mut self, target: f32) {
        self.target = target;
    }

    /// Jumps to `value`.
    pub fn reset(&mut self, value: f32) {
        self.value = value;
        self.target = value;
    }

    /// Current target.
    pub fn target(&self) -> f32 {
        self.target
    }

    /// Next smoothed value.
    #[inline]
    pub fn next_value(&mut self) -> f32 {
        self.value += (self.target - self.value) * self.coef;
        self.value
    }
}
