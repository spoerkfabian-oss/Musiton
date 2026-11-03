//! Lock-free state shared between the UI thread and the audio thread.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

/// Scalars read and written from both the UI and the audio thread (atomics only).
#[derive(Debug, Default)]
pub struct EngineShared {
    test_tone: AtomicBool,
    xruns: AtomicU64,
    cpu_load_bits: AtomicU32,
}

impl EngineShared {
    /// Creates a new shared state with everything off/zero.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Turns the 440 Hz test tone on or off.
    pub fn set_test_tone(&self, on: bool) {
        self.test_tone.store(on, Ordering::Relaxed);
    }

    /// Whether the test tone is requested.
    pub fn test_tone(&self) -> bool {
        self.test_tone.load(Ordering::Relaxed)
    }

    /// Records one buffer under-/overrun (dropout).
    pub fn add_xrun(&self) {
        self.xruns.fetch_add(1, Ordering::Relaxed);
    }

    /// Number of dropouts since start (or the last [`Self::reset_xruns`]).
    pub fn xruns(&self) -> u64 {
        self.xruns.load(Ordering::Relaxed)
    }

    /// Resets the dropout counter.
    pub fn reset_xruns(&self) {
        self.xruns.store(0, Ordering::Relaxed);
    }

    /// Stores the audio-thread load (1.0 = the whole buffer period was used).
    pub fn set_cpu_load(&self, load: f32) {
        self.cpu_load_bits.store(load.to_bits(), Ordering::Relaxed);
    }

    /// Audio-thread load (1.0 = 100 %).
    pub fn cpu_load(&self) -> f32 {
        f32::from_bits(self.cpu_load_bits.load(Ordering::Relaxed))
    }
}
