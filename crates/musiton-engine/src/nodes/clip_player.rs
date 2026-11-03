//! Plays the audio clips of one track.

use std::sync::Arc;

use musiton_core::audio::AudioData;
use musiton_core::Tick;

use crate::node::{AudioIo, AudioNode, Event, ProcessContext};

/// One clip as the engine sees it.
#[derive(Debug, Clone)]
pub struct PlayerClip {
    /// Timeline start.
    pub start: Tick,
    /// Timeline end (exclusive).
    pub end: Tick,
    /// First frame of the file that is played (at the file's rate).
    pub source_offset: u64,
    /// Linear gain.
    pub gain: f32,
    /// Fade-in length.
    pub fade_in: Tick,
    /// Fade-out length.
    pub fade_out: Tick,
    /// The decoded file.
    pub audio: Arc<AudioData>,
}

/// Stereo output: sums all clips that overlap the current block.
/// Mono files go to both channels. Files at another rate than the engine are played with
/// linear interpolation (normally all files are converted to the project rate on import).
#[derive(Debug, Default)]
pub struct ClipPlayer {
    clips: Vec<PlayerClip>,
}

impl ClipPlayer {
    /// Player for `clips`.
    pub fn new(clips: Vec<PlayerClip>) -> Self {
        Self { clips }
    }
}

impl AudioNode for ClipPlayer {
    fn num_inputs(&self) -> usize {
        0
    }

    fn num_outputs(&self) -> usize {
        2
    }

    fn prepare(&mut self, _sample_rate: f32, _max_block: usize) {}

    fn process(&mut self, ctx: &ProcessContext, io: &mut AudioIo, _events: &[Event]) {
        for o in io.outputs() {
            o.fill(0.0);
        }
        if !ctx.playing {
            return;
        }
        let sr = f64::from(ctx.sample_rate);
        let pos = ctx.sample_pos as i64;
        let n = ctx.frames as i64;
        let at = |t: Tick| ctx.tempo.tick_to_sample(t.0 as f64, sr).round() as i64;
        for clip in &self.clips {
            let cs = at(clip.start);
            let ce = at(clip.end);
            if ce <= pos || cs >= pos + n || ce <= cs {
                continue;
            }
            let fade_in = (at(Tick(clip.start.0 + clip.fade_in.0)) - cs).max(0);
            let fade_out = (ce - at(Tick(clip.end.0 - clip.fade_out.0))).max(0);
            let len = ce - cs;
            let ratio = f64::from(clip.audio.sample_rate) / sr;
            let left = clip.audio.channel(0);
            let right = if clip.audio.num_channels() > 1 {
                clip.audio.channel(1)
            } else {
                left
            };
            let from = (cs - pos).max(0);
            let to = (ce - pos).min(n);
            for i in from..to {
                let t = pos + i - cs;
                let mut g = clip.gain;
                if t < fade_in {
                    g *= t as f32 / fade_in as f32;
                }
                let remaining = len - t;
                if remaining <= fade_out {
                    g *= (remaining - 1).max(0) as f32 / fade_out as f32;
                }
                let (l, r) = if ratio == 1.0 {
                    let s = clip.source_offset as usize + t as usize;
                    (
                        left.get(s).copied().unwrap_or(0.0),
                        right.get(s).copied().unwrap_or(0.0),
                    )
                } else {
                    let s = clip.source_offset as f64 + t as f64 * ratio;
                    (lerp(left, s), lerp(right, s))
                };
                let idx = i as usize;
                if let Some(o) = io.output(0).get_mut(idx) {
                    *o += l * g;
                }
                if let Some(o) = io.output(1).get_mut(idx) {
                    *o += r * g;
                }
            }
        }
    }

    fn reset(&mut self) {}
}

#[inline]
fn lerp(x: &[f32], pos: f64) -> f32 {
    let i = pos.floor() as usize;
    let frac = (pos - pos.floor()) as f32;
    let a = x.get(i).copied().unwrap_or(0.0);
    let b = x.get(i + 1).copied().unwrap_or(0.0);
    a + (b - a) * frac
}
