//! Musical and audio time: ticks, sample positions, tempo map, time signature.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Ticks per quarter note.
pub const PPQ: i64 = 960;

/// A musical position in ticks ([`PPQ`] per quarter note).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Tick(pub i64);

/// An absolute position in samples.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct SamplePos(pub u64);

/// Lowest and highest tempo accepted (BPM).
pub const BPM_RANGE: (f64, f64) = (10.0, 999.0);

/// A tempo change (step, no ramp) at `tick`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TempoPoint {
    /// Where the tempo starts.
    pub tick: Tick,
    /// Beats (quarter notes) per minute.
    pub bpm: f64,
}

/// Tempo changes over time with tick ↔ seconds ↔ samples conversion.
///
/// Lookups are binary searches over a cached table: no allocation, safe on the audio thread.
/// Serialized as its list of points.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "Vec<TempoPoint>", into = "Vec<TempoPoint>")]
pub struct TempoMap {
    points: Vec<TempoPoint>,
    /// Seconds elapsed at each point.
    secs: Vec<f64>,
}

impl Default for TempoMap {
    fn default() -> Self {
        Self::constant(120.0)
    }
}

impl TempoMap {
    /// A single tempo for the whole song.
    pub fn constant(bpm: f64) -> Self {
        Self::from_points(vec![TempoPoint { tick: Tick(0), bpm }])
    }

    /// Builds a map from arbitrary points: sorted, BPM clamped to [`BPM_RANGE`],
    /// later duplicates of a tick win, and a point at tick 0 is guaranteed.
    pub fn from_points(mut points: Vec<TempoPoint>) -> Self {
        for p in &mut points {
            p.bpm = if p.bpm.is_finite() {
                p.bpm.clamp(BPM_RANGE.0, BPM_RANGE.1)
            } else {
                120.0
            };
            p.tick.0 = p.tick.0.max(0);
        }
        points.sort_by_key(|p| p.tick);
        points.reverse();
        points.dedup_by_key(|p| p.tick);
        points.reverse();
        if points.first().is_none_or(|p| p.tick.0 != 0) {
            let bpm = points.first().map_or(120.0, |p| p.bpm);
            points.insert(0, TempoPoint { tick: Tick(0), bpm });
        }
        let mut secs = Vec::with_capacity(points.len());
        let mut acc = 0.0;
        for (i, p) in points.iter().enumerate() {
            if let Some(prev) = i.checked_sub(1).and_then(|j| points.get(j)) {
                acc += (p.tick.0 - prev.tick.0) as f64 * secs_per_tick(prev.bpm);
            }
            secs.push(acc);
        }
        Self { points, secs }
    }

    /// The tempo points (first one is at tick 0).
    pub fn points(&self) -> &[TempoPoint] {
        &self.points
    }

    /// Tempo at `tick`.
    pub fn bpm_at(&self, tick: f64) -> f64 {
        self.segment_by_tick(tick).1.bpm
    }

    /// Seconds from song start to `tick` (negative ticks extrapolate the first tempo).
    pub fn tick_to_seconds(&self, tick: f64) -> f64 {
        let (secs, p) = self.segment_by_tick(tick);
        secs + (tick - p.tick.0 as f64) * secs_per_tick(p.bpm)
    }

    /// Tick at `seconds` from song start.
    pub fn seconds_to_tick(&self, seconds: f64) -> f64 {
        let i = self
            .secs
            .partition_point(|&s| s <= seconds)
            .saturating_sub(1);
        let (secs, p) = self.segment(i);
        p.tick.0 as f64 + (seconds - secs) / secs_per_tick(p.bpm)
    }

    /// Sample position (fractional) of `tick` at `sample_rate`.
    pub fn tick_to_sample(&self, tick: f64, sample_rate: f64) -> f64 {
        self.tick_to_seconds(tick) * sample_rate
    }

    /// Tick (fractional) at sample position `sample`.
    pub fn sample_to_tick(&self, sample: f64, sample_rate: f64) -> f64 {
        if sample_rate <= 0.0 {
            return 0.0;
        }
        self.seconds_to_tick(sample / sample_rate)
    }

    fn segment_by_tick(&self, tick: f64) -> (f64, TempoPoint) {
        let i = self
            .points
            .partition_point(|p| p.tick.0 as f64 <= tick)
            .saturating_sub(1);
        self.segment(i)
    }

    fn segment(&self, i: usize) -> (f64, TempoPoint) {
        let p = self.points.get(i).copied().unwrap_or(TempoPoint {
            tick: Tick(0),
            bpm: 120.0,
        });
        (self.secs.get(i).copied().unwrap_or(0.0), p)
    }
}

impl From<Vec<TempoPoint>> for TempoMap {
    fn from(points: Vec<TempoPoint>) -> Self {
        Self::from_points(points)
    }
}

impl From<TempoMap> for Vec<TempoPoint> {
    fn from(map: TempoMap) -> Self {
        map.points
    }
}

#[inline]
fn secs_per_tick(bpm: f64) -> f64 {
    60.0 / (bpm * PPQ as f64)
}

/// A time signature such as 4/4 or 6/8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TimeSig {
    /// Beats per bar (1..=32).
    pub num: u8,
    /// Beat unit: 1, 2, 4, 8, 16 or 32.
    pub den: u8,
}

impl Default for TimeSig {
    fn default() -> Self {
        Self { num: 4, den: 4 }
    }
}

impl TimeSig {
    /// Creates a time signature, clamping to valid values.
    pub fn new(num: u8, den: u8) -> Self {
        let den = [1u8, 2, 4, 8, 16, 32]
            .into_iter()
            .min_by_key(|d| d.abs_diff(den))
            .unwrap_or(4);
        Self {
            num: num.clamp(1, 32),
            den,
        }
    }

    /// Ticks per beat (beat unit = `den`).
    pub fn ticks_per_beat(&self) -> i64 {
        PPQ * 4 / i64::from(self.den.max(1))
    }

    /// Ticks per bar.
    pub fn ticks_per_bar(&self) -> i64 {
        self.ticks_per_beat() * i64::from(self.num)
    }
}

/// A position as bar.beat.tick, 1-based bar and beat (e.g. `1.1.000` = song start).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarBeatTick {
    /// Bar, starting at 1.
    pub bar: i64,
    /// Beat within the bar, starting at 1.
    pub beat: i64,
    /// Ticks within the beat.
    pub tick: i64,
}

impl BarBeatTick {
    /// Converts a tick position using a single time signature.
    pub fn from_tick(tick: Tick, sig: TimeSig) -> Self {
        let bar_len = sig.ticks_per_bar();
        let beat_len = sig.ticks_per_beat();
        let bar = tick.0.div_euclid(bar_len);
        let in_bar = tick.0.rem_euclid(bar_len);
        Self {
            bar: bar + 1,
            beat: in_bar / beat_len + 1,
            tick: in_bar % beat_len,
        }
    }
}

impl fmt::Display for BarBeatTick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{:03}", self.bar, self.beat, self.tick)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;
    use proptest::prelude::*;

    #[test]
    fn constant_120_bpm() {
        let m = TempoMap::constant(120.0);
        assert_relative_eq!(m.tick_to_seconds(PPQ as f64), 0.5);
        assert_relative_eq!(m.tick_to_sample(4.0 * PPQ as f64, 48_000.0), 96_000.0);
        assert_relative_eq!(m.sample_to_tick(24_000.0, 48_000.0), PPQ as f64);
    }

    #[test]
    fn step_change() {
        // 1 bar (4 beats) at 120 = 2 s, then 60 BPM: one beat = 1 s.
        let m = TempoMap::from_points(vec![
            TempoPoint {
                tick: Tick(0),
                bpm: 120.0,
            },
            TempoPoint {
                tick: Tick(4 * PPQ),
                bpm: 60.0,
            },
        ]);
        assert_relative_eq!(m.tick_to_seconds(5.0 * PPQ as f64), 3.0);
        assert_relative_eq!(m.seconds_to_tick(3.0), 5.0 * PPQ as f64);
        assert_relative_eq!(m.bpm_at(4.0 * PPQ as f64), 60.0);
        assert_relative_eq!(m.bpm_at(4.0 * PPQ as f64 - 1.0), 120.0);
    }

    #[test]
    fn from_points_normalises() {
        let m = TempoMap::from_points(vec![
            TempoPoint {
                tick: Tick(960),
                bpm: 90.0,
            },
            TempoPoint {
                tick: Tick(960),
                bpm: 100.0,
            },
            TempoPoint {
                tick: Tick(0),
                bpm: 5000.0,
            },
        ]);
        assert_eq!(m.points().len(), 2);
        assert_eq!(m.points()[0].bpm, BPM_RANGE.1);
        assert_eq!(m.points()[1].bpm, 100.0);
        assert_eq!(TempoMap::from_points(vec![]).points()[0].tick, Tick(0));
    }

    #[test]
    fn bar_beat_tick() {
        let s = TimeSig::new(4, 4);
        assert_eq!(BarBeatTick::from_tick(Tick(0), s).to_string(), "1.1.000");
        assert_eq!(
            BarBeatTick::from_tick(Tick(4 * PPQ + PPQ + 5), s).to_string(),
            "2.2.005"
        );
        let s68 = TimeSig::new(6, 8);
        assert_eq!(s68.ticks_per_beat(), 480);
        assert_eq!(s68.ticks_per_bar(), 2880);
        assert_eq!(TimeSig::new(3, 5).den, 4);
    }

    fn arb_map() -> impl Strategy<Value = TempoMap> {
        prop::collection::vec((0i64..200_000, 20.0f64..300.0), 0..8).prop_map(|v| {
            TempoMap::from_points(
                v.into_iter()
                    .map(|(t, bpm)| TempoPoint { tick: Tick(t), bpm })
                    .collect(),
            )
        })
    }

    proptest! {
        #[test]
        fn tick_sample_round_trip(m in arb_map(), tick in 0.0f64..1_000_000.0) {
            let s = m.tick_to_sample(tick, 48_000.0);
            let back = m.sample_to_tick(s, 48_000.0);
            prop_assert!((back - tick).abs() < 1e-6 * tick.max(1.0));
        }

        #[test]
        fn monotonic(m in arb_map(), a in 0.0f64..500_000.0, d in 0.001f64..500_000.0) {
            prop_assert!(m.tick_to_seconds(a + d) > m.tick_to_seconds(a));
        }
    }
}
