//! Horizontal zoom/scroll of the timeline.

use musiton_core::{TimeSig, PPQ};

/// Narrowest zoom: pixels per tick (1 bar of 4/4 ≈ 8 px).
pub const MIN_PX_PER_TICK: f32 = 8.0 / (4.0 * PPQ as f32);
/// Widest zoom: pixels per tick (1 bar of 4/4 ≈ 8000 px).
pub const MAX_PX_PER_TICK: f32 = 8000.0 / (4.0 * PPQ as f32);

/// Maps ticks to pixels for the timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimelineView {
    /// Zoom.
    pub px_per_tick: f32,
    /// Tick at the left edge of the lanes.
    pub scroll: f64,
}

impl Default for TimelineView {
    fn default() -> Self {
        Self {
            px_per_tick: 120.0 / (4.0 * PPQ as f32),
            scroll: 0.0,
        }
    }
}

impl TimelineView {
    /// X offset (from the lane's left edge) of `tick`.
    pub fn x(&self, tick: f64) -> f32 {
        ((tick - self.scroll) * f64::from(self.px_per_tick)) as f32
    }

    /// Tick at x offset `x`.
    pub fn tick(&self, x: f32) -> f64 {
        self.scroll + f64::from(x / self.px_per_tick)
    }

    /// Zooms by `factor`, keeping the tick under `anchor_x` in place.
    pub fn zoom(&mut self, factor: f32, anchor_x: f32) {
        let anchor = self.tick(anchor_x);
        self.px_per_tick = (self.px_per_tick * factor).clamp(MIN_PX_PER_TICK, MAX_PX_PER_TICK);
        self.scroll = (anchor - f64::from(anchor_x / self.px_per_tick)).max(0.0);
    }

    /// Scrolls by `dx` pixels (positive = later).
    pub fn scroll_px(&mut self, dx: f32) {
        self.scroll = (self.scroll + f64::from(dx / self.px_per_tick)).max(0.0);
    }

    /// Keeps `tick` visible in a lane `width` px wide by paging when it leaves the view.
    pub fn follow(&mut self, tick: f64, width: f32) {
        let x = self.x(tick);
        if x < 0.0 || x > width * 0.95 {
            self.scroll = (tick - f64::from(width * 0.05 / self.px_per_tick)).max(0.0);
        }
    }

    /// Bars between numbered ruler labels so that labels are at least `min_px` apart.
    pub fn bar_label_step(&self, sig: TimeSig, min_px: f32) -> i64 {
        let bar_px = sig.ticks_per_bar() as f32 * self.px_per_tick;
        let mut step = 1;
        while (step as f32) * bar_px < min_px && step < 1 << 16 {
            step *= 2;
        }
        step
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_round_trips() {
        let v = TimelineView {
            px_per_tick: 0.05,
            scroll: 1000.0,
        };
        assert_eq!(v.x(1000.0), 0.0);
        assert!((v.tick(v.x(5000.0)) - 5000.0).abs() < 1e-6);
    }

    #[test]
    fn zoom_keeps_anchor_and_clamps() {
        let mut v = TimelineView {
            scroll: 3840.0,
            ..TimelineView::default()
        };
        let before = v.tick(200.0);
        v.zoom(2.0, 200.0);
        assert!((v.tick(200.0) - before).abs() < 1e-3);
        v.zoom(1e9, 0.0);
        assert_eq!(v.px_per_tick, MAX_PX_PER_TICK);
        v.zoom(1e-9, 0.0);
        assert_eq!(v.px_per_tick, MIN_PX_PER_TICK);
    }

    #[test]
    fn follow_pages_forward() {
        let mut v = TimelineView::default();
        v.follow(100.0, 1000.0);
        assert_eq!(v.scroll, 0.0);
        let far = 100.0 * 3840.0;
        v.follow(far, 1000.0);
        let x = v.x(far);
        assert!((0.0..100.0).contains(&x));
    }

    #[test]
    fn label_step_grows_when_zoomed_out() {
        let mut v = TimelineView::default();
        let sig = TimeSig::default();
        assert_eq!(v.bar_label_step(sig, 40.0), 1);
        v.px_per_tick = MIN_PX_PER_TICK;
        assert!(v.bar_label_step(sig, 40.0) >= 4);
    }
}
