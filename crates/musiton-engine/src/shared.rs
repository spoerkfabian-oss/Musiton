//! Lock-free status shared between the UI thread and the audio thread.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

/// Scalars read and written from both threads (atomics only).
#[derive(Debug)]
pub struct EngineShared {
    test_tone: AtomicBool,
    xruns: AtomicU64,
    cpu_load_bits: AtomicU32,
    playing: AtomicBool,
    playhead: AtomicU64,
    probe_request: AtomicBool,
    /// Last latency measurement: frames, or [`PROBE_NONE`] / [`PROBE_TIMEOUT`].
    probe_result: AtomicI64,
    recording: AtomicBool,
    /// Last controller seen on the MIDI input: 1<<24 | ch<<16 | cc<<8 | value (0 = none).
    last_cc: AtomicU32,
}

/// No latency measurement yet.
pub const PROBE_NONE: i64 = -1;
/// The measurement click was not heard on the input within one second.
pub const PROBE_TIMEOUT: i64 = -2;

impl Default for EngineShared {
    fn default() -> Self {
        Self {
            test_tone: AtomicBool::new(false),
            xruns: AtomicU64::new(0),
            cpu_load_bits: AtomicU32::new(0),
            playing: AtomicBool::new(false),
            playhead: AtomicU64::new(0),
            probe_request: AtomicBool::new(false),
            probe_result: AtomicI64::new(PROBE_NONE),
            recording: AtomicBool::new(false),
            last_cc: AtomicU32::new(0),
        }
    }
}

impl EngineShared {
    /// Creates a new shared state with everything off/zero.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Starts a round-trip latency measurement: a click is played and timed until it
    /// arrives at input channel 1 (needs a cable or a microphone near the speaker).
    pub fn request_latency_probe(&self) {
        self.probe_result.store(PROBE_NONE, Ordering::Relaxed);
        self.probe_request.store(true, Ordering::Relaxed);
    }

    pub(crate) fn take_probe_request(&self) -> bool {
        self.probe_request.swap(false, Ordering::Relaxed)
    }

    pub(crate) fn set_probe_result(&self, v: i64) {
        self.probe_result.store(v, Ordering::Relaxed);
    }

    /// Result of the last latency measurement (frames, [`PROBE_NONE`] or [`PROBE_TIMEOUT`]).
    pub fn probe_result(&self) -> i64 {
        self.probe_result.load(Ordering::Relaxed)
    }

    pub(crate) fn publish_recording(&self, on: bool) {
        self.recording.store(on, Ordering::Relaxed);
    }

    pub(crate) fn set_last_cc(&self, ch: u8, cc: u8, value: u8) {
        let v = 1 << 24 | u32::from(ch) << 16 | u32::from(cc) << 8 | u32::from(value);
        self.last_cc.store(v, Ordering::Relaxed);
    }

    /// Last controller moved on the MIDI input: (channel, controller, value 0..127).
    /// The groundwork for MIDI learn.
    pub fn last_cc(&self) -> Option<(u8, u8, u8)> {
        let v = self.last_cc.load(Ordering::Relaxed);
        (v >> 24 == 1).then_some(((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }

    /// Whether recording is armed in the engine.
    pub fn recording(&self) -> bool {
        self.recording.load(Ordering::Relaxed)
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

    pub(crate) fn publish_transport(&self, playing: bool, playhead: u64) {
        self.playing.store(playing, Ordering::Relaxed);
        self.playhead.store(playhead, Ordering::Relaxed);
    }

    /// Whether the transport is running (as of the last processed block).
    pub fn playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    /// Song position in samples (as of the last processed block).
    pub fn playhead(&self) -> u64 {
        self.playhead.load(Ordering::Relaxed)
    }
}
