//! Drawing a clip: body, name, waveform, fade lines.

use egui::{Color32, FontId, Mesh, Painter, Pos2, Rect, Stroke};
use musiton_core::audio::Peaks;
use musiton_core::TempoMap;
use musiton_project::Clip;

use crate::view::TimelineView;

/// Height of the clip title strip.
pub const TITLE_HEIGHT: f32 = 14.0;

/// Everything needed to draw one clip.
pub struct ClipPaint<'a> {
    /// The clip.
    pub clip: &'a Clip,
    /// Clip rectangle on screen (may extend beyond the lane).
    pub rect: Rect,
    /// Visible part of the lane.
    pub clip_to: Rect,
    /// Track colour.
    pub color: [u8; 3],
    /// Selected clips are highlighted.
    pub selected: bool,
    /// Display name.
    pub name: &'a str,
    /// Peaks of the clip's audio, if loaded.
    pub peaks: Option<&'a Peaks>,
    /// Timeline mapping.
    pub view: &'a TimelineView,
    /// Left edge of the lane on screen.
    pub lane_left: f32,
    /// Tempo (for timeline ↔ file position).
    pub tempo: &'a TempoMap,
}

/// Paints a clip.
pub fn paint_clip(painter: &Painter, p: &ClipPaint) {
    let [r, g, b] = p.color;
    let base = Color32::from_rgb(r, g, b);
    let body = base.gamma_multiply(0.35);
    let rect = p.rect;
    painter.rect_filled(rect, 3.0, body);
    let title = Rect::from_min_max(rect.min, Pos2::new(rect.right(), rect.top() + TITLE_HEIGHT));
    painter.rect_filled(title, 3.0, base.gamma_multiply(0.75));
    let stroke = if p.selected {
        Stroke::new(2.0, Color32::WHITE)
    } else {
        Stroke::new(1.0, base)
    };
    painter.rect_stroke(rect, 3.0, stroke, egui::StrokeKind::Inside);
    let text_x = rect.left().max(p.clip_to.left()) + 4.0;
    painter.with_clip_rect(rect.intersect(p.clip_to)).text(
        Pos2::new(text_x, rect.top() + 1.0),
        egui::Align2::LEFT_TOP,
        p.name,
        FontId::proportional(11.0),
        Color32::BLACK,
    );

    if let Clip::Audio(a) = p.clip {
        let wave = Rect::from_min_max(Pos2::new(rect.left(), title.bottom() + 1.0), rect.max);
        if let Some(peaks) = p.peaks {
            paint_waveform(painter, p, a, peaks, wave, base);
        }
        // Fade lines.
        let fade_color = Color32::from_white_alpha(180);
        if a.fade_in.0 > 0 {
            let x = p.lane_left + p.view.x((a.start.0 + a.fade_in.0) as f64);
            painter.line_segment(
                [
                    Pos2::new(rect.left(), wave.bottom()),
                    Pos2::new(x, wave.top()),
                ],
                Stroke::new(1.0, fade_color),
            );
        }
        if a.fade_out.0 > 0 {
            let x = p.lane_left + p.view.x((a.start.0 + a.length.0 - a.fade_out.0) as f64);
            painter.line_segment(
                [
                    Pos2::new(x, wave.top()),
                    Pos2::new(rect.right(), wave.bottom()),
                ],
                Stroke::new(1.0, fade_color),
            );
        }
        // Fade handles.
        for x in [
            p.lane_left + p.view.x((a.start.0 + a.fade_in.0) as f64),
            p.lane_left + p.view.x((a.start.0 + a.length.0 - a.fade_out.0) as f64),
        ] {
            painter.rect_filled(
                Rect::from_center_size(Pos2::new(x, title.bottom()), egui::vec2(7.0, 7.0)),
                1.0,
                Color32::WHITE,
            );
        }
    }
}

fn paint_waveform(
    painter: &Painter,
    p: &ClipPaint,
    a: &musiton_project::AudioClip,
    peaks: &Peaks,
    rect: Rect,
    color: Color32,
) {
    let sr = f64::from(peaks.sample_rate.max(1));
    let start_secs = p.tempo.tick_to_seconds(a.start.0 as f64);
    let visible = rect.intersect(p.clip_to);
    if visible.width() <= 0.0 || peaks.frames == 0 {
        return;
    }
    let frame_at = |x: f32| -> f64 {
        let tick = p.view.tick(x - p.lane_left);
        a.source_offset.0 as f64 + (p.tempo.tick_to_seconds(tick) - start_secs) * sr
    };
    let frames_per_px = (frame_at(visible.left() + 1.0) - frame_at(visible.left())).abs();
    let Some((level, bucket)) = peaks.level_for(frames_per_px) else {
        return;
    };
    let gain = musiton_core::units::db_to_gain(a.gain_db);
    let mid = rect.center().y;
    let half = rect.height() * 0.5 - 1.0;
    let mut mesh = Mesh::default();
    let mix = |c: u8| ((u16::from(c) + 255) / 2) as u8;
    let wave_color = Color32::from_rgb(mix(color.r()), mix(color.g()), mix(color.b()));
    let mut x = visible.left();
    while x < visible.right() {
        let f0 = frame_at(x).max(0.0);
        let f1 = frame_at(x + 1.0).max(f0 + 1.0);
        if f0 >= peaks.frames as f64 {
            break;
        }
        let b0 = (f0 as usize) / bucket;
        let b1 = ((f1 as usize) / bucket).max(b0 + 1);
        let (mut lo, mut hi) = (0.0_f32, 0.0_f32);
        for ch in level {
            for &(l, h) in ch.get(b0..b1.min(ch.len())).unwrap_or(&[]) {
                lo = lo.min(l);
                hi = hi.max(h);
            }
        }
        let top = mid - (hi * gain).clamp(-1.0, 1.0) * half;
        let bottom = mid - (lo * gain).clamp(-1.0, 1.0) * half;
        mesh.add_colored_rect(
            Rect::from_min_max(Pos2::new(x, top), Pos2::new(x + 1.0, bottom.max(top + 1.0))),
            wave_color,
        );
        x += 1.0;
    }
    painter.add(mesh);
}
