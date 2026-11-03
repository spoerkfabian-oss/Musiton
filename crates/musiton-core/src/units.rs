//! Unit conversions.

/// Decibels to linear gain. −∞ dB (or anything ≤ −144 dB) gives 0.
#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    if db <= -144.0 {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

/// Linear gain to decibels. 0 gives −∞.
#[inline]
pub fn gain_to_db(gain: f32) -> f32 {
    20.0 * gain.abs().log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_round_trip() {
        for db in [-60.0_f32, -12.0, -6.0, 0.0, 6.0] {
            assert!((gain_to_db(db_to_gain(db)) - db).abs() < 1e-4);
        }
        assert!((db_to_gain(-6.0206) - 0.5).abs() < 1e-4);
        assert_eq!(db_to_gain(f32::NEG_INFINITY), 0.0);
        assert_eq!(gain_to_db(0.0), f32::NEG_INFINITY);
    }
}
