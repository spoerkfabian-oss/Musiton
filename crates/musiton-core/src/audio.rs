//! Decoded audio in memory and waveform peaks.

/// Planar audio: one `Vec` per channel, all the same length.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AudioData {
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Samples per channel.
    pub channels: Vec<Vec<f32>>,
}

impl AudioData {
    /// Number of frames (samples per channel).
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    /// Number of channels.
    pub fn num_channels(&self) -> usize {
        self.channels.len()
    }

    /// Channel `ch` (empty if missing).
    pub fn channel(&self, ch: usize) -> &[f32] {
        self.channels.get(ch).map_or(&[], Vec::as_slice)
    }
}

/// Frames per bucket of the finest peak level; each further level is 8× coarser.
pub const PEAK_BASE: usize = 64;
/// Factor between peak levels.
pub const PEAK_FACTOR: usize = 8;

/// One peak level: per channel, (min, max) per bucket.
pub type PeakLevel = Vec<Vec<(f32, f32)>>;

/// Min/max pairs at several resolutions, for drawing waveforms at any zoom.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Peaks {
    /// Sample rate of the analysed audio.
    pub sample_rate: u32,
    /// Length of the analysed audio in frames.
    pub frames: usize,
    /// `levels[l][ch][i]` = (min, max) of bucket `i`; level `l` has
    /// `PEAK_BASE * PEAK_FACTOR^l` frames per bucket.
    pub levels: Vec<PeakLevel>,
}

impl Peaks {
    /// Computes all levels down to a handful of buckets.
    pub fn compute(audio: &AudioData) -> Self {
        let mut levels = Vec::new();
        let first: PeakLevel = audio
            .channels
            .iter()
            .map(|ch| {
                ch.chunks(PEAK_BASE)
                    .map(|c| {
                        c.iter()
                            .fold((f32::MAX, f32::MIN), |(lo, hi), &s| (lo.min(s), hi.max(s)))
                    })
                    .collect()
            })
            .collect();
        levels.push(first);
        while levels
            .last()
            .and_then(|l| l.first())
            .is_some_and(|c| c.len() > PEAK_FACTOR)
        {
            let Some(prev) = levels.last() else {
                break;
            };
            let next = prev
                .iter()
                .map(|ch| {
                    ch.chunks(PEAK_FACTOR)
                        .map(|c| {
                            c.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &(a, b)| {
                                (lo.min(a), hi.max(b))
                            })
                        })
                        .collect()
                })
                .collect();
            levels.push(next);
        }
        Self {
            sample_rate: audio.sample_rate,
            frames: audio.frames(),
            levels,
        }
    }

    /// The coarsest level whose buckets are at most `frames_per_px` frames wide, with its
    /// bucket size. `None` if there is no data.
    pub fn level_for(&self, frames_per_px: f64) -> Option<(&PeakLevel, usize)> {
        let mut best = None;
        let mut size = PEAK_BASE;
        for level in &self.levels {
            if best.is_some() && size as f64 > frames_per_px {
                break;
            }
            best = Some((level, size));
            size *= PEAK_FACTOR;
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peaks_cover_all_levels() {
        let samples: Vec<f32> = (0..100_000)
            .map(|i| ((i % 100) as f32 / 50.0) - 1.0)
            .collect();
        let audio = AudioData {
            sample_rate: 48_000,
            channels: vec![samples.clone(), samples.iter().map(|s| s * 0.5).collect()],
        };
        let p = Peaks::compute(&audio);
        assert_eq!(p.levels[0][0].len(), 100_000usize.div_ceil(64));
        assert!(p.levels.last().unwrap()[0].len() <= PEAK_FACTOR);
        let (lo, hi) = p.levels.last().unwrap()[0][0];
        assert_eq!((lo, hi), (-1.0, 0.98));
        let (lo, hi) = p.levels[2][1][3];
        assert!(lo >= -0.5 && hi <= 0.5);

        let (_, size) = p.level_for(10.0).unwrap();
        assert_eq!(size, 64);
        let (_, size) = p.level_for(5000.0).unwrap();
        assert_eq!(size, 4096);
        assert!(Peaks::default().level_for(1.0).is_none());
    }
}
