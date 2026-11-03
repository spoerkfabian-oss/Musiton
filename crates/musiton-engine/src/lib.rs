//! Real-time audio engine for Musiton.
//!
//! The engine renders interleaved `f32` audio. It is driven either by a device
//! callback (see `musiton-io`) or offline by [`render_offline`] — both run the same code.

mod engine;
pub mod rt;
mod shared;

pub use engine::{render_offline, Engine, TEST_TONE_HZ, TEST_TONE_LEVEL};
pub use shared::EngineShared;
