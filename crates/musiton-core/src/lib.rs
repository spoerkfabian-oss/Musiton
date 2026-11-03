//! Shared types for Musiton: musical time, IDs, units. Depends on no other Musiton crate.

pub mod ids;
pub mod time;
pub mod units;

pub use time::{BarBeatTick, SamplePos, TempoMap, TempoPoint, Tick, TimeSig, PPQ};
