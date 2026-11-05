//! Parameter automation on the audio thread.

use musiton_core::ids::ParamId;
use musiton_core::params::{curve_shape, ParamInfo};

use crate::graph::NodeIdx;

/// Automation is applied at sub-blocks of this many samples, aligned to the song position
/// (so renders don't depend on the device buffer size).
pub const AUTOMATION_BLOCK: u64 = 32;

/// One automated parameter as the engine sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoLane {
    /// Node in the running graph.
    pub node: NodeIdx,
    /// Parameter of that node.
    pub param: ParamId,
    /// Maps the normalized lane values to plain parameter values.
    pub info: ParamInfo,
    /// The plain value is in dB and the node wants a linear gain (track volume).
    pub db_to_gain: bool,
    /// (tick, normalized value, curve to the next point), sorted by tick.
    pub points: Vec<(f64, f32, f32)>,
}

impl AutoLane {
    /// Plain parameter value at `tick` (`None` without points).
    pub fn value_at(&self, tick: f64) -> Option<f32> {
        let first = self.points.first()?;
        let i = self.points.partition_point(|p| p.0 <= tick);
        let norm = if i == 0 {
            first.1
        } else {
            let a = self.points.get(i - 1)?;
            match self.points.get(i) {
                Some(b) => {
                    let x = ((tick - a.0) / (b.0 - a.0).max(1.0)) as f32;
                    a.1 + (b.1 - a.1) * curve_shape(x, a.2)
                }
                None => a.1,
            }
        };
        let v = self.info.from_normalized(norm);
        Some(if self.db_to_gain {
            if v <= -144.0 {
                0.0
            } else {
                10f32.powf(v / 20.0)
            }
        } else {
            v
        })
    }
}

/// All automation for the running graph.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Automation(pub Vec<AutoLane>);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_maps_through_the_parameter_scale() {
        let lane = AutoLane {
            node: NodeIdx(0),
            param: ParamId(8),
            info: ParamInfo::new(8, "Cutoff", 20.0, 20_000.0, 1000.0).log(),
            db_to_gain: false,
            points: vec![(0.0, 0.0, 0.0), (100.0, 1.0, 0.0)],
        };
        assert_eq!(lane.value_at(-1.0), Some(20.0));
        assert!((lane.value_at(50.0).unwrap() - 632.46).abs() < 0.1);
        assert!((lane.value_at(200.0).unwrap() - 20_000.0).abs() < 0.5);
        let vol = AutoLane {
            info: ParamInfo::new(0, "Volume", -60.0, 6.0, 0.0),
            db_to_gain: true,
            points: vec![(0.0, 60.0 / 66.0, 0.0)],
            ..lane
        };
        assert!((vol.value_at(0.0).unwrap() - 1.0).abs() < 1e-4);
    }
}
