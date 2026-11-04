//! Real-time safety helpers.
//!
//! In debug builds, code run through [`no_alloc`] aborts the process on any heap
//! allocation, provided the binary installs [`AllocDisabler`] as its global allocator.

/// Global allocator that enforces [`no_alloc`] (debug builds).
#[cfg(debug_assertions)]
pub use assert_no_alloc::AllocDisabler;
/// In release builds the checks are off and this is the normal system allocator.
#[cfg(not(debug_assertions))]
pub use std::alloc::System as AllocDisabler;

/// Runs `f` with allocation checks enabled (debug builds only).
///
/// Wrap every audio-thread entry point (e.g. the device callback) in this.
#[inline]
pub fn no_alloc<R>(f: impl FnOnce() -> R) -> R {
    #[cfg(debug_assertions)]
    {
        assert_no_alloc::assert_no_alloc(f)
    }
    #[cfg(not(debug_assertions))]
    {
        f()
    }
}
