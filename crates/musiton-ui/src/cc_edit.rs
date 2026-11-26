//! Editing the controller changes of a MIDI clip (pure functions, used by the piano roll).

use musiton_core::Tick;
use musiton_project::CcEvent;

/// Controllers offered by name in the piano roll: (number, name). Pitch bend is carried as
/// controller 129 (centre 64).
pub const NAMED: [(u8, &str); 6] = [
    (1, "Mod wheel"),
    (7, "Volume"),
    (10, "Pan"),
    (11, "Expression"),
    (64, "Sustain pedal"),
    // Same number as `musiton_engine::node::PITCH_BEND` (the UI does not depend on the engine).
    (129, "Pitch bend"),
];

/// Display name of a controller, e.g. "Mod wheel (CC 1)", "Pitch bend" or "CC 74".
pub fn name(controller: u8) -> String {
    match NAMED.iter().find(|(n, _)| *n == controller) {
        Some((n, name)) if *n < 128 => format!("{name} (CC {n})"),
        Some((_, name)) => (*name).to_owned(),
        None => format!("CC {controller}"),
    }
}

/// The value `controller` has at `tick` (the last change at or before it).
pub fn value_at(events: &[CcEvent], controller: u8, tick: i64) -> Option<u8> {
    events
        .iter()
        .filter(|e| e.controller == controller && e.tick.0 <= tick)
        .max_by_key(|e| e.tick)
        .map(|e| e.value)
}

/// Draws a line for `controller` from `from` to `to` (tick, value): one change per `step`
/// ticks on the grid, replacing the controller's existing changes in that span.
/// Positions are kept inside `0..clip_len`. Repeated values are not stored twice.
pub fn paint(
    events: &mut Vec<CcEvent>,
    controller: u8,
    from: (i64, u8),
    to: (i64, u8),
    step: i64,
    clip_len: i64,
) {
    let step = step.max(1);
    let last = (clip_len - 1).max(0);
    let grid = |t: i64| ((t as f64 / step as f64).round() as i64 * step).clamp(0, last);
    let (a, b) = if from.0 <= to.0 {
        (from, to)
    } else {
        (to, from)
    };
    let (t0, t1) = (grid(a.0), grid(b.0));
    events.retain(|e| !(e.controller == controller && (t0..=t1).contains(&e.tick.0)));
    let mut previous = value_at(events, controller, t0 - 1);
    let mut t = t0;
    loop {
        let x = if t1 > t0 {
            (t - t0) as f64 / (t1 - t0) as f64
        } else {
            1.0
        };
        let value = (f64::from(a.1) + (f64::from(b.1) - f64::from(a.1)) * x).round() as u8;
        if previous != Some(value) {
            events.push(CcEvent {
                tick: Tick(t),
                controller,
                value: value.min(127),
                channel: 0,
            });
            previous = Some(value);
        }
        if t >= t1 {
            break;
        }
        t = (t + step).min(t1);
    }
    events.sort_by_key(|e| (e.tick, e.controller));
}

/// Removes the changes of `controller` between `t0` and `t1` (inclusive, any order).
pub fn erase(events: &mut Vec<CcEvent>, controller: u8, t0: i64, t1: i64) {
    let (a, b) = (t0.min(t1), t0.max(t1));
    events.retain(|e| !(e.controller == controller && (a..=b).contains(&e.tick.0)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticks(events: &[CcEvent], controller: u8) -> Vec<(i64, u8)> {
        events
            .iter()
            .filter(|e| e.controller == controller)
            .map(|e| (e.tick.0, e.value))
            .collect()
    }

    #[test]
    fn painting_draws_a_ramp_on_the_grid_and_replaces_what_was_there() {
        let mut ev = vec![
            CcEvent {
                tick: Tick(100),
                controller: 1,
                value: 5,
                channel: 0,
            },
            CcEvent {
                tick: Tick(100),
                controller: 7,
                value: 99,
                channel: 0,
            },
        ];
        paint(&mut ev, 1, (10, 0), (470, 120), 240, 960);
        // Grid 0, 240, 480 → values 0, 60, 120; the old change at 100 is gone.
        assert_eq!(ticks(&ev, 1), [(0, 0), (240, 60), (480, 120)]);
        // Other controllers are untouched.
        assert_eq!(ticks(&ev, 7), [(100, 99)]);
        assert_eq!(value_at(&ev, 1, 300), Some(60));
        assert_eq!(value_at(&ev, 1, -1), None);

        // Right to left works the same; a flat stroke stores one change only.
        paint(&mut ev, 1, (480, 64), (0, 64), 240, 960);
        assert_eq!(ticks(&ev, 1), [(0, 64)]);

        // Positions stay inside the clip.
        paint(&mut ev, 1, (5_000, 10), (5_000, 10), 240, 960);
        assert_eq!(ticks(&ev, 1), [(0, 64), (959, 10)]);

        erase(&mut ev, 1, 2_000, 500);
        assert_eq!(ticks(&ev, 1), [(0, 64)]);
    }

    #[test]
    fn names() {
        assert_eq!(name(1), "Mod wheel (CC 1)");
        assert_eq!(name(74), "CC 74");
    }
}
