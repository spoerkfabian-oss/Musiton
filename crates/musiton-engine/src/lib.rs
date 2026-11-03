//! Real-time audio engine for Musiton.
//!
//! [`create`] returns an [`EngineHandle`] (UI thread) and an [`Engine`] (audio thread).
//! The engine is driven either by a device callback (see `musiton-io`) or offline by
//! [`render_offline`] — both run exactly the same code.

mod command;
mod engine;
pub mod graph;
pub mod node;
pub mod nodes;
pub mod rt;
mod shared;

pub use command::{EngineCommand, InputSource};
pub use engine::{create, render_offline, Engine, EngineHandle, TEST_TONE_HZ, TEST_TONE_LEVEL};
pub use shared::{EngineShared, PROBE_NONE, PROBE_TIMEOUT};
