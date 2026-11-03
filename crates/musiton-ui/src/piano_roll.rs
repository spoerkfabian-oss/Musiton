//! Piano roll editor for one MIDI clip.

use std::collections::HashSet;

use egui::{Color32, Key, Modifiers, Pos2, Rect, Sense, Stroke, Ui};
use musiton_core::ids::{ClipId, TrackId};
use musiton_core::{Tick, TimeSig};
use musiton_project::{Clip, Edit, MidiClip, Note, Project};

use crate::note_edit;
use crate::snap::Snap;
use crate::view::TimelineView;
use crate::UiAction;

const KEYBOARD_WIDTH: f32 = 44.0;
const VELOCITY_HEIGHT: f32 = 60.0;
const KEY_HEIGHT: f32 = 12.0;
const EDGE: f32 = 6.0;

/// Editing tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tool {
    /// Click to add notes, drag to set their length.
    #[default]
    Draw,
    /// Click to select, drag on empty space for a selection rectangle.
    Select,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum DragKind {
    Move { grabbed: usize },
    Resize { grabbed: usize },
    Box,
    Velocity { index: usize },
}

#[derive(Debug)]
struct Drag {
    kind: DragKind,
    origin: Pos2,
    orig: Vec<Note>,
    idx: Vec<usize>,
}

/// Piano roll state.
#[derive(Debug)]
pub struct PianoRoll {
    /// The clip being edited.
    pub clip: Option<(TrackId, ClipId)>,
    view: TimelineView,
    /// Highest key visible at the top.
    top_key: i32,
    tool: Tool,
    snap: Snap,
    strength: f32,
    selected: HashSet<usize>,
    drag: Option<Drag>,
    working: Option<Vec<Note>>,
    preview_key: Option<u8>,
    last_len: i64,
    /// Scroll to the clip's notes on the next frame.
    center: bool,
}

impl Default for PianoRoll {
    fn default() -> Self {
        Self {
            clip: None,
            view: TimelineView {
                px_per_tick: 0.12,
                scroll: 0.0,
            },
            top_key: 84,
            tool: Tool::Draw,
            snap: Snap::Note {
                n: 16,
                triplet: false,
            },
            strength: 1.0,
            selected: HashSet::new(),
            drag: None,
            working: None,
            preview_key: None,
            last_len: 240,
            center: false,
        }
    }
}

impl PianoRoll {
    /// Opens `clip` for editing.
    pub fn open(&mut self, track: TrackId, clip: ClipId) {
        if self.clip != Some((track, clip)) {
            self.clip = Some((track, clip));
            self.selected.clear();
            self.drag = None;
            self.working = None;
            self.view.scroll = 0.0;
            self.center = true;
        }
    }

    /// Draws the editor.
    pub fn ui(&mut self, ui: &mut Ui, project: &Project) -> Vec<UiAction> {
        let mut actions = Vec::new();
        let found = self.clip.and_then(|(t, c)| {
            let clip = project.track(t)?.clips.iter().find(|x| x.id() == c)?;
            match clip {
                Clip::Midi(m) => Some((t, m.clone())),
                Clip::Audio(_) => None,
            }
        });
        let Some((track, clip)) = found else {
            ui.weak("Double-click an instrument track to create a MIDI clip, or double-click a MIDI clip to edit it.");
            return actions;
        };
        let sig = project.time_sig;
        if std::mem::take(&mut self.center) {
            let (lo, hi) = clip.notes.iter().fold((127, 0), |(lo, hi), n| {
                (lo.min(i32::from(n.pitch)), hi.max(i32::from(n.pitch)))
            });
            let visible = ((ui.available_height() - VELOCITY_HEIGHT) / KEY_HEIGHT) as i32;
            self.top_key = if lo > hi {
                84
            } else {
                ((lo + hi) / 2 + visible / 2).clamp(20, 127)
            };
        }
        self.toolbar(ui, track, &clip, sig, &mut actions);
        self.shortcuts(ui, track, &clip, sig, &mut actions);

        let area = ui.available_rect_before_wrap();
        let grid = Rect::from_min_max(
            Pos2::new(area.left() + KEYBOARD_WIDTH, area.top()),
            Pos2::new(area.right(), area.bottom() - VELOCITY_HEIGHT),
        );
        let vel_rect = Rect::from_min_max(Pos2::new(grid.left(), grid.bottom()), area.max);
        let keys = Rect::from_min_max(area.min, Pos2::new(grid.left(), grid.bottom()));
        ui.allocate_rect(area, Sense::hover());

        if ui.rect_contains_pointer(area) {
            let (zoom, delta, pointer) =
                ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.pointer.hover_pos()));
            if zoom != 1.0 {
                let anchor = pointer.map_or(0.0, |p| p.x - grid.left()).max(0.0);
                self.view.zoom(zoom, anchor);
            }
            if delta.x != 0.0 {
                self.view.scroll_px(-delta.x);
            }
            if delta.y != 0.0 {
                self.top_key =
                    (self.top_key + (delta.y / KEY_HEIGHT).round() as i32).clamp(20, 127);
            }
        }

        let notes = self.working.clone().unwrap_or_else(|| clip.notes.clone());
        self.keyboard(ui, keys, &mut actions);
        self.grid_ui(ui, grid, track, &clip, &notes, sig, &mut actions);
        self.velocity_ui(ui, vel_rect, &notes);
        self.finish_drag(ui, grid, vel_rect, track, &clip, sig, &mut actions);
        actions
    }

    fn y_of(&self, grid: Rect, key: i32) -> f32 {
        grid.top() + (self.top_key - key) as f32 * KEY_HEIGHT
    }

    fn key_at(&self, grid: Rect, y: f32) -> i32 {
        self.top_key - ((y - grid.top()) / KEY_HEIGHT).floor() as i32
    }

    fn note_rect(&self, grid: Rect, n: &Note) -> Rect {
        let x0 = grid.left() + self.view.x(n.start.0 as f64);
        let x1 = grid.left() + self.view.x((n.start.0 + n.length.0) as f64);
        let y = self.y_of(grid, i32::from(n.pitch));
        Rect::from_min_max(
            Pos2::new(x0, y + 1.0),
            Pos2::new(x1.max(x0 + 3.0), y + KEY_HEIGHT - 1.0),
        )
    }

    /// Index of the topmost note under `p`, and whether `p` is on its right edge.
    fn hit(&self, grid: Rect, notes: &[Note], p: Pos2) -> Option<(usize, bool)> {
        notes.iter().enumerate().rev().find_map(|(i, n)| {
            let r = self.note_rect(grid, n);
            r.contains(p).then(|| (i, p.x > r.right() - EDGE))
        })
    }

    fn toolbar(
        &mut self,
        ui: &mut Ui,
        track: TrackId,
        clip: &MidiClip,
        sig: TimeSig,
        actions: &mut Vec<UiAction>,
    ) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tool, Tool::Draw, "Draw")
                .on_hover_text("Click to add notes (right-click deletes)");
            ui.selectable_value(&mut self.tool, Tool::Select, "Select")
                .on_hover_text("Drag a rectangle to select");
            ui.separator();
            ui.label("Grid");
            egui::ComboBox::from_id_salt("pr_snap")
                .width(60.0)
                .selected_text(self.snap.label())
                .show_ui(ui, |ui| {
                    for s in Snap::ALL {
                        ui.selectable_value(&mut self.snap, s, s.label());
                    }
                });
            if ui
                .button("Quantize")
                .on_hover_text("Q — selected notes, or all")
                .clicked()
            {
                self.quantize(track, clip, sig, actions);
            }
            ui.add(
                egui::Slider::new(&mut self.strength, 0.0..=1.0)
                    .text("strength")
                    .show_value(false),
            )
            .on_hover_text(format!("{:.0} %", self.strength * 100.0));
            ui.separator();
            for (label, st) in [("-12", -12), ("-1", -1), ("+1", 1), ("+12", 12)] {
                if ui
                    .small_button(label)
                    .on_hover_text("Transpose selected notes (↑/↓, Shift = octave)")
                    .clicked()
                {
                    self.transpose(track, clip, st, actions);
                }
            }
            ui.separator();
            ui.weak(format!(
                "{} notes, {} selected",
                clip.notes.len(),
                self.selected.len()
            ));
        });
    }

    fn sel(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self.selected.iter().copied().collect();
        v.sort_unstable();
        v
    }

    fn sel_or_all(&self, clip: &MidiClip) -> Vec<usize> {
        if self.selected.is_empty() {
            (0..clip.notes.len()).collect()
        } else {
            self.sel()
        }
    }

    fn commit(track: TrackId, clip: &MidiClip, notes: Vec<Note>) -> UiAction {
        let mut c = clip.clone();
        c.notes = notes;
        UiAction::Edit {
            edit: Edit::ReplaceClip {
                track,
                clip: Clip::Midi(c),
            },
            merge: false,
        }
    }

    fn quantize(
        &mut self,
        track: TrackId,
        clip: &MidiClip,
        sig: TimeSig,
        actions: &mut Vec<UiAction>,
    ) {
        let mut notes = clip.notes.clone();
        note_edit::quantize(
            &mut notes,
            &self.sel_or_all(clip),
            self.snap,
            sig,
            self.strength,
        );
        actions.push(Self::commit(track, clip, notes));
    }

    fn transpose(&mut self, track: TrackId, clip: &MidiClip, st: i32, actions: &mut Vec<UiAction>) {
        if self.selected.is_empty() {
            return;
        }
        let mut notes = clip.notes.clone();
        note_edit::transpose(&mut notes, &self.sel(), st);
        actions.push(Self::commit(track, clip, notes));
    }

    fn shortcuts(
        &mut self,
        ui: &mut Ui,
        track: TrackId,
        clip: &MidiClip,
        sig: TimeSig,
        actions: &mut Vec<UiAction>,
    ) {
        let pointer_here = ui.rect_contains_pointer(ui.available_rect_before_wrap());
        if !pointer_here || crate::text_input_focused(ui.ctx()) {
            return;
        }
        let sc = |m, k| ui.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(m, k)));
        if sc(Modifiers::COMMAND, Key::A) {
            self.selected = (0..clip.notes.len()).collect();
        }
        if sc(Modifiers::NONE, Key::Delete) && !self.selected.is_empty() {
            let notes = clip
                .notes
                .iter()
                .enumerate()
                .filter(|(i, _)| !self.selected.contains(i))
                .map(|(_, n)| *n)
                .collect();
            self.selected.clear();
            actions.push(Self::commit(track, clip, notes));
        }
        if sc(Modifiers::COMMAND, Key::D) && !self.selected.is_empty() {
            let mut notes = clip.notes.clone();
            let new = note_edit::duplicate(&mut notes, &self.sel());
            self.selected = new.into_iter().collect();
            actions.push(Self::commit(track, clip, notes));
        }
        if sc(Modifiers::NONE, Key::Q) {
            self.quantize(track, clip, sig, actions);
        }
        for (m, k, st) in [
            (Modifiers::SHIFT, Key::ArrowUp, 12),
            (Modifiers::SHIFT, Key::ArrowDown, -12),
            (Modifiers::NONE, Key::ArrowUp, 1),
            (Modifiers::NONE, Key::ArrowDown, -1),
        ] {
            if sc(m, k) {
                self.transpose(track, clip, st, actions);
            }
        }
    }

    fn keyboard(&mut self, ui: &mut Ui, rect: Rect, actions: &mut Vec<UiAction>) {
        let painter = ui.painter_at(rect);
        let resp = ui.interact(rect, ui.scope_id().with("pr_keys"), Sense::click_and_drag());
        let grid_like = Rect::from_min_max(Pos2::new(rect.right(), rect.top()), rect.max);
        let bottom_key = self.key_at(grid_like, rect.bottom());
        for k in bottom_key.max(0)..=self.top_key.min(127) {
            let y = self.y_of(grid_like, k);
            let r = Rect::from_min_max(
                Pos2::new(rect.left(), y),
                Pos2::new(rect.right(), y + KEY_HEIGHT),
            );
            let black = matches!(k % 12, 1 | 3 | 6 | 8 | 10);
            let pressed = self.preview_key == Some(k as u8);
            let fill = match (pressed, black) {
                (true, _) => Color32::from_rgb(255, 210, 80),
                (false, true) => Color32::from_gray(30),
                (false, false) => Color32::from_gray(200),
            };
            painter.rect_filled(r.shrink2(egui::vec2(0.0, 0.5)), 0.0, fill);
            if k % 12 == 0 {
                painter.text(
                    Pos2::new(r.left() + 2.0, r.center().y),
                    egui::Align2::LEFT_CENTER,
                    format!("C{}", k / 12 - 1),
                    egui::FontId::proportional(9.0),
                    Color32::BLACK,
                );
            }
        }
        if resp.is_pointer_button_down_on() {
            if let Some(p) = resp.interact_pointer_pos() {
                let key = self.key_at(grid_like, p.y).clamp(0, 127) as u8;
                if self.preview_key != Some(key) {
                    if let Some(old) = self.preview_key.take() {
                        actions.push(UiAction::PreviewNote {
                            on: false,
                            key: old,
                        });
                    }
                    actions.push(UiAction::PreviewNote { on: true, key });
                    self.preview_key = Some(key);
                }
            }
        } else if self.drag.is_none() {
            if let Some(old) = self.preview_key.take() {
                actions.push(UiAction::PreviewNote {
                    on: false,
                    key: old,
                });
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn grid_ui(
        &mut self,
        ui: &mut Ui,
        grid: Rect,
        track: TrackId,
        clip: &MidiClip,
        notes: &[Note],
        sig: TimeSig,
        actions: &mut Vec<UiAction>,
    ) {
        let painter = ui.painter_at(grid);
        painter.rect_filled(grid, 0.0, Color32::from_gray(24));
        let bottom_key = self.key_at(grid, grid.bottom());
        for k in bottom_key.max(0)..=self.top_key.min(127) {
            if matches!(k % 12, 1 | 3 | 6 | 8 | 10) {
                let y = self.y_of(grid, k);
                painter.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(grid.left(), y),
                        Pos2::new(grid.right(), y + KEY_HEIGHT),
                    ),
                    0.0,
                    Color32::from_gray(18),
                );
            }
            if k % 12 == 0 {
                let y = self.y_of(grid, k) + KEY_HEIGHT;
                painter.hline(grid.x_range(), y, Stroke::new(1.0, Color32::from_gray(45)));
            }
        }
        // Vertical grid: snap lines, beats, bars.
        let step = self.snap.step(sig).unwrap_or(sig.ticks_per_beat()).max(1);
        let step = if step as f32 * self.view.px_per_tick < 6.0 {
            sig.ticks_per_beat()
        } else {
            step
        };
        let mut t = (self.view.scroll as i64 / step) * step;
        while grid.left() + self.view.x(t as f64) < grid.right() {
            let x = grid.left() + self.view.x(t as f64);
            let c = if t % sig.ticks_per_bar() == 0 {
                65
            } else if t % sig.ticks_per_beat() == 0 {
                45
            } else {
                32
            };
            painter.vline(x, grid.y_range(), Stroke::new(1.0, Color32::from_gray(c)));
            t += step;
        }
        let end_x = grid.left() + self.view.x(clip.length.0 as f64);
        painter.rect_filled(
            Rect::from_min_max(Pos2::new(end_x.max(grid.left()), grid.top()), grid.max),
            0.0,
            Color32::from_black_alpha(90),
        );
        painter.vline(
            end_x,
            grid.y_range(),
            Stroke::new(2.0, Color32::from_gray(140)),
        );

        for (i, n) in notes.iter().enumerate() {
            let r = self.note_rect(grid, n);
            if !r.intersects(grid) {
                continue;
            }
            let v = f32::from(n.velocity) / 127.0;
            let fill = Color32::from_rgb((80.0 + 150.0 * v) as u8, (140.0 + 60.0 * v) as u8, 255);
            painter.rect_filled(r, 2.0, fill);
            if self.selected.contains(&i) {
                painter.rect_stroke(
                    r,
                    2.0,
                    Stroke::new(1.5, Color32::WHITE),
                    egui::StrokeKind::Inside,
                );
            }
        }
        if let Some(Drag {
            kind: DragKind::Box,
            origin,
            ..
        }) = &self.drag
        {
            if let Some(p) = ui.input(|i| i.pointer.latest_pos()) {
                let r = Rect::from_two_pos(*origin, p);
                painter.rect_filled(r, 0.0, Color32::from_white_alpha(20));
                painter.rect_stroke(
                    r,
                    0.0,
                    Stroke::new(1.0, Color32::WHITE),
                    egui::StrokeKind::Inside,
                );
            }
        }

        let resp = ui.interact(grid, ui.scope_id().with("pr_grid"), Sense::click_and_drag());
        if let Some(p) = resp.hover_pos() {
            if self.hit(grid, notes, p).is_some_and(|(_, edge)| edge) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
        }
        let shift = ui.input(|i| i.modifiers.shift || i.modifiers.command);
        let view = self.view;
        let tick_at = |p: Pos2| view.tick(p.x - grid.left()).max(0.0);
        if resp.secondary_clicked() {
            if let Some((i, _)) = resp
                .interact_pointer_pos()
                .and_then(|p| self.hit(grid, notes, p))
            {
                let mut v = clip.notes.clone();
                if i < v.len() {
                    v.remove(i);
                }
                self.selected.clear();
                actions.push(Self::commit(track, clip, v));
            }
        }
        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                match self.hit(grid, notes, p) {
                    Some((i, _)) => self.select(i, shift),
                    None if self.tool == Tool::Draw => {
                        let mut v = clip.notes.clone();
                        v.push(self.new_note(tick_at(p), self.key_at(grid, p.y), sig));
                        self.selected = [v.len() - 1].into_iter().collect();
                        actions.push(Self::commit(track, clip, v));
                    }
                    None => self.selected.clear(),
                }
            }
        }
        if resp.drag_started() {
            if let Some(p) = resp.interact_pointer_pos() {
                let mut orig = clip.notes.clone();
                let kind = match self.hit(grid, notes, p) {
                    Some((i, edge)) => {
                        if !self.selected.contains(&i) {
                            self.select(i, shift);
                        }
                        if let Some(n) = orig.get(i) {
                            actions.push(UiAction::PreviewNote {
                                on: true,
                                key: n.pitch,
                            });
                            self.preview_key = Some(n.pitch);
                        }
                        if edge {
                            DragKind::Resize { grabbed: i }
                        } else {
                            DragKind::Move { grabbed: i }
                        }
                    }
                    None if self.tool == Tool::Draw => {
                        orig.push(self.new_note(tick_at(p), self.key_at(grid, p.y), sig));
                        let i = orig.len() - 1;
                        self.selected = [i].into_iter().collect();
                        DragKind::Resize { grabbed: i }
                    }
                    None => {
                        if !shift {
                            self.selected.clear();
                        }
                        DragKind::Box
                    }
                };
                self.drag = Some(Drag {
                    kind,
                    origin: p,
                    orig,
                    idx: self.sel(),
                });
            }
        }
    }

    fn new_note(&self, tick: f64, key: i32, sig: TimeSig) -> Note {
        Note {
            pitch: key.clamp(0, 127) as u8,
            velocity: 100,
            start: self.snap.floor(tick, sig),
            length: Tick(
                self.snap
                    .step(sig)
                    .unwrap_or(self.last_len)
                    .max(note_edit::MIN_NOTE),
            ),
            channel: 0,
        }
    }

    fn select(&mut self, i: usize, additive: bool) {
        if additive {
            if !self.selected.remove(&i) {
                self.selected.insert(i);
            }
        } else {
            self.selected.clear();
            self.selected.insert(i);
        }
    }

    fn velocity_ui(&mut self, ui: &mut Ui, rect: Rect, notes: &[Note]) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_gray(16));
        painter.hline(
            rect.x_range(),
            rect.top(),
            Stroke::new(1.0, Color32::from_gray(60)),
        );
        for (i, n) in notes.iter().enumerate() {
            let x = rect.left() + self.view.x(n.start.0 as f64);
            if !rect.x_range().contains(x) {
                continue;
            }
            let h = (rect.height() - 4.0) * f32::from(n.velocity) / 127.0;
            let c = if self.selected.contains(&i) {
                Color32::WHITE
            } else {
                Color32::from_rgb(120, 170, 255)
            };
            painter.line_segment(
                [Pos2::new(x, rect.bottom()), Pos2::new(x, rect.bottom() - h)],
                Stroke::new(3.0, c),
            );
        }
        let resp = ui.interact(rect, ui.scope_id().with("pr_vel"), Sense::drag());
        if resp.drag_started() {
            if let Some(p) = resp.interact_pointer_pos() {
                let near = notes
                    .iter()
                    .enumerate()
                    .map(|(i, n)| (i, (rect.left() + self.view.x(n.start.0 as f64) - p.x).abs()))
                    .filter(|(_, d)| *d < 6.0)
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                if let Some((i, _)) = near {
                    let idx = if self.selected.contains(&i) {
                        self.sel()
                    } else {
                        vec![i]
                    };
                    self.drag = Some(Drag {
                        kind: DragKind::Velocity { index: i },
                        origin: p,
                        orig: notes.to_vec(),
                        idx,
                    });
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_drag(
        &mut self,
        ui: &Ui,
        grid: Rect,
        vel: Rect,
        track: TrackId,
        clip: &MidiClip,
        sig: TimeSig,
        actions: &mut Vec<UiAction>,
    ) {
        let Some(d) = &self.drag else {
            return;
        };
        let (pos, down) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.primary_down()));
        let p = pos.unwrap_or(d.origin);
        let dt = f64::from((p.x - d.origin.x) / self.view.px_per_tick);
        let working = match d.kind {
            DragKind::Move { .. } => {
                let dp = self.key_at(grid, p.y) - self.key_at(grid, d.origin.y);
                Some(note_edit::moved(&d.orig, &d.idx, dt, dp, self.snap, sig))
            }
            DragKind::Resize { grabbed } => Some(note_edit::resized(
                &d.orig, &d.idx, grabbed, dt, self.snap, sig,
            )),
            DragKind::Velocity { .. } => {
                let v = ((vel.bottom() - p.y) / (vel.height() - 4.0) * 127.0)
                    .round()
                    .clamp(1.0, 127.0) as u8;
                let mut out = d.orig.clone();
                for &i in &d.idx {
                    if let Some(n) = out.get_mut(i) {
                        n.velocity = v;
                    }
                }
                Some(out)
            }
            DragKind::Box => None,
        };
        if down {
            self.working = working;
            return;
        }
        let Some(d) = self.drag.take() else {
            return;
        };
        self.working = None;
        if let Some(old) = self.preview_key.take() {
            actions.push(UiAction::PreviewNote {
                on: false,
                key: old,
            });
        }
        match d.kind {
            DragKind::Box => {
                let r = Rect::from_two_pos(d.origin, p);
                let t0 = self.view.tick(r.left() - grid.left()) as i64;
                let t1 = self.view.tick(r.right() - grid.left()) as i64;
                let p1 = self.key_at(grid, r.top()).clamp(0, 127) as u8;
                let p0 = self.key_at(grid, r.bottom()).clamp(0, 127) as u8;
                self.selected
                    .extend(note_edit::in_rect(&clip.notes, t0, t1, p0, p1));
            }
            DragKind::Resize { grabbed } => {
                if let Some(n) = working.as_ref().and_then(|w| w.get(grabbed)) {
                    self.last_len = n.length.0;
                }
                if let Some(w) = working {
                    actions.push(Self::commit(track, clip, w));
                }
            }
            _ => {
                if let Some(w) = working.filter(|w| *w != clip.notes) {
                    actions.push(Self::commit(track, clip, w));
                }
            }
        }
    }
}
