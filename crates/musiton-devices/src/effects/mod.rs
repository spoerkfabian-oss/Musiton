//! Built-in audio effects (stereo in, stereo out; the compressor also has a sidechain).

// Per-channel loops over fixed `[_; 2]` arrays read more clearly with an index.
#![allow(clippy::needless_range_loop)]

pub mod compressor;
pub mod delay;
pub mod eq;
pub mod limiter;
pub mod reverb;
pub mod saturation;
pub mod utility;

use musiton_core::params::ParamInfo;

/// Stores a parameter value clamped to its range; returns `false` for unknown ids.
pub(crate) fn store(values: &mut [f32], params: &[ParamInfo], id: u64, v: f32) -> bool {
    match (values.get_mut(id as usize), params.get(id as usize)) {
        (Some(slot), Some(info)) => {
            *slot = v.clamp(info.min.min(info.max), info.max.max(info.min));
            true
        }
        _ => false,
    }
}

/// dB to linear gain.
#[inline]
pub(crate) fn db(x: f32) -> f32 {
    10f32.powf(x / 20.0)
}
