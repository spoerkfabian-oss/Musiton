//! Real-time safety helpers.
//!
//! In debug builds, code run through [`no_alloc`] aborts the process on any heap
//! allocation, provided the binary installs [`AllocDisabler`] as its global allocator.

pub use assert_no_alloc::AllocDisabler;

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
