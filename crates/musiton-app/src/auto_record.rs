//! Recording control moves into automation lanes (Write, Touch, Latch).

use std::collections::HashMap;

use musiton_core::ids::TrackId;
use musiton_core::Tick;
use musiton_project::{AutoMode, AutoPoint, AutomationLane, Edit, ParamTarget, Project};

/// Smallest value change kept when thinning recorded points.
const THIN: f32 = 0.003;

#[derive(Debug, Clone)]
struct Take {
    mode: AutoMode,
    points: Vec<AutoPoint>,
    value: f32,
    /// Touch mode: the control is held.
    held: bool,
}

/// Collects control moves per lane while the transport runs.
#[derive(Debug, Default)]
pub struct AutoRecorder {
    takes: HashMap<(TrackId, ParamTarget), Take>,
}

impl AutoRecorder {
    /// Lanes being recorded (their playback is suspended).
    pub fn active(&self) -> Vec<(TrackId, ParamTarget)> {
        self.takes.keys().copied().collect()
    }

    /// The mode of `target`'s lane if it records control moves.
    pub fn recording_mode(
        project: &Project,
        track: TrackId,
        target: ParamTarget,
    ) -> Option<AutoMode> {
        let lane = project
            .track(track)?
            .automation
            .iter()
            .find(|l| l.target == target)?;
        matches!(
            lane.mode,
            AutoMode::Write | AutoMode::Touch | AutoMode::Latch
        )
        .then_some(lane.mode)
    }

    /// The user moved a control (normalized `value`) at `tick` while playing.
    pub fn touch(
        &mut self,
        project: &Project,
        track: TrackId,
        target: ParamTarget,
        value: f32,
        tick: Tick,
    ) {
        let Some(mode) = Self::recording_mode(project, track, target) else {
            return;
        };
        let take = self.takes.entry((track, target)).or_insert(Take {
            mode,
            points: Vec::new(),
            value,
            held: true,
        });
        take.value = value;
        take.held = true;
        take.points.push(AutoPoint {
            tick,
            value,
            curve: 0.0,
        });
    }

    /// Write-mode lanes start recording their current value when playback starts.
    pub fn start_write_lanes(
        &mut self,
        project: &Project,
        current: &dyn Fn(TrackId, ParamTarget) -> f32,
        tick: Tick,
    ) {
        for t in &project.tracks {
            for lane in t.automation.iter().filter(|l| l.mode == AutoMode::Write) {
                let value = current(t.id, lane.target);
                self.takes.entry((t.id, lane.target)).or_insert(Take {
                    mode: AutoMode::Write,
                    points: vec![AutoPoint {
                        tick,
                        value,
                        curve: 0.0,
                    }],
                    value,
                    held: false,
                });
            }
        }
    }

    /// Called every UI frame while playing: Latch/Write keep writing their last value,
    /// Touch takes end when the mouse is released. Returns edits for finished takes.
    pub fn frame(&mut self, project: &Project, tick: Tick, pointer_down: bool) -> Vec<Edit> {
        let mut done = Vec::new();
        for (key, take) in &mut self.takes {
            match take.mode {
                AutoMode::Touch if !pointer_down => done.push(*key),
                AutoMode::Touch => {}
                _ => {
                    if take.points.last().is_none_or(|p| p.tick < tick) {
                        take.points.push(AutoPoint {
                            tick,
                            value: take.value,
                            curve: 0.0,
                        });
                    }
                }
            }
            if !pointer_down {
                take.held = false;
            }
        }
        done.into_iter()
            .filter_map(|k| self.finish(project, k))
            .collect()
    }

    /// Playback stopped: every take is written.
    pub fn stop(&mut self, project: &Project) -> Vec<Edit> {
        let keys: Vec<_> = self.takes.keys().copied().collect();
        keys.into_iter()
            .filter_map(|k| self.finish(project, k))
            .collect()
    }

    fn finish(&mut self, project: &Project, key: (TrackId, ParamTarget)) -> Option<Edit> {
        let take = self.takes.remove(&key)?;
        let t = project.track(key.0)?;
        let mut lanes = t.automation.clone();
        let lane = lanes.iter_mut().find(|l| l.target == key.1)?;
        let new = merge(lane, &thin(&take.points), take.mode == AutoMode::Touch);
        lane.points = new;
        Some(Edit::SetAutomation {
            track: key.0,
            lanes,
        })
    }
}

/// Drops points that lie on the line between their neighbours (within [`THIN`]).
pub fn thin(points: &[AutoPoint]) -> Vec<AutoPoint> {
    let mut out: Vec<AutoPoint> = Vec::new();
    for (i, p) in points.iter().enumerate() {
        let next = points.get(i + 1);
        match (out.last(), next) {
            (Some(a), Some(b)) => {
                let span = (b.tick.0 - a.tick.0).max(1) as f32;
                let x = (p.tick.0 - a.tick.0) as f32 / span;
                let line = a.value + (b.value - a.value) * x;
                if (line - p.value).abs() > THIN {
                    out.push(*p);
                }
            }
            _ => out.push(*p),
        }
    }
    out
}

/// Replaces the lane's points inside the recorded range. A touch take returns to the old
/// automation right after it ends.
pub fn merge(lane: &AutomationLane, recorded: &[AutoPoint], return_after: bool) -> Vec<AutoPoint> {
    let (Some(first), Some(last)) = (recorded.first(), recorded.last()) else {
        return lane.points.clone();
    };
    let (a, b) = (first.tick, last.tick);
    let mut out: Vec<AutoPoint> = lane
        .points
        .iter()
        .filter(|p| p.tick < a || p.tick > b)
        .copied()
        .collect();
    if let Some(before) = lane
        .value_at(a.0 as f64 - 1.0)
        .filter(|_| lane.points.iter().any(|p| p.tick < a))
    {
        out.push(AutoPoint {
            tick: Tick(a.0 - 1),
            value: before,
            curve: 0.0,
        });
    }
    out.extend_from_slice(recorded);
    if return_after {
        if let Some(after) = lane
            .value_at(b.0 as f64 + 1.0)
            .filter(|_| !lane.points.is_empty())
        {
            out.push(AutoPoint {
                tick: Tick(b.0 + 1),
                value: after,
                curve: 0.0,
            });
        }
    }
    out.sort_by_key(|p| p.tick);
    out.dedup_by_key(|p| p.tick);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use musiton_project::{Track, UndoStack};

    fn project(mode: AutoMode) -> Project {
        let mut p = Project::default();
        p.tracks.push(Track {
            id: TrackId(1),
            automation: vec![AutomationLane {
                target: ParamTarget::Volume,
                points: vec![AutoPoint {
                    tick: Tick(0),
                    value: 0.5,
                    curve: 0.0,
                }],
                mode,
            }],
            ..Track::default()
        });
        p
    }

    fn lane(p: &Project) -> &AutomationLane {
        &p.tracks[0].automation[0]
    }

    #[test]
    fn touch_records_while_held_and_returns_afterwards() {
        let mut p = project(AutoMode::Touch);
        let mut r = AutoRecorder::default();
        for (k, v) in [(1000, 0.6), (1100, 0.7), (1200, 0.8)] {
            r.touch(&p, TrackId(1), ParamTarget::Volume, v, Tick(k));
            assert!(r.frame(&p, Tick(k), true).is_empty());
        }
        assert_eq!(r.active().len(), 1);
        let edits = r.frame(&p, Tick(1300), false);
        assert_eq!(edits.len(), 1);
        for e in edits {
            UndoStack::new().perform(&mut p, e, false).unwrap();
        }
        let l = lane(&p);
        assert!((l.value_at(500.0).unwrap() - 0.5).abs() < 1e-6);
        assert!((l.value_at(1100.0).unwrap() - 0.7).abs() < 1e-6);
        assert!(
            (l.value_at(5000.0).unwrap() - 0.5).abs() < 1e-6,
            "returns after the touch"
        );
    }

    #[test]
    fn latch_keeps_the_last_value_until_stop() {
        let mut p = project(AutoMode::Latch);
        let mut r = AutoRecorder::default();
        r.touch(&p, TrackId(1), ParamTarget::Volume, 0.9, Tick(1000));
        r.frame(&p, Tick(1000), false);
        r.frame(&p, Tick(4000), false);
        for e in r.stop(&p) {
            UndoStack::new().perform(&mut p, e, false).unwrap();
        }
        assert!((lane(&p).value_at(3000.0).unwrap() - 0.9).abs() < 1e-6);
        // Reading mode lanes are not recorded.
        let p2 = project(AutoMode::Read);
        let mut r2 = AutoRecorder::default();
        r2.touch(&p2, TrackId(1), ParamTarget::Volume, 0.1, Tick(0));
        assert!(r2.active().is_empty());
    }

    #[test]
    fn write_records_from_the_start_and_thinning_removes_straight_runs() {
        let mut p = project(AutoMode::Write);
        let mut r = AutoRecorder::default();
        r.start_write_lanes(&p, &|_, _| 0.25, Tick(0));
        for k in 1..100 {
            r.frame(&p, Tick(k * 10), false);
        }
        for e in r.stop(&p) {
            UndoStack::new().perform(&mut p, e, false).unwrap();
        }
        let l = lane(&p);
        assert!(l.points.len() <= 3, "thinned to {} points", l.points.len());
        assert!((l.value_at(500.0).unwrap() - 0.25).abs() < 1e-6);
    }
}
