//! Arrangement view: toolbar, bar/beat ruler, track headers and timeline lanes.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use egui::{Align2, Color32, FontId, Key, Modifiers, Pos2, Rect, Sense, Stroke, Ui, UiBuilder};
use musiton_core::audio::Peaks;
use musiton_core::ids::{AssetId, ClipId, TrackId};
use musiton_core::params::ParamInfo;
use musiton_core::{Tick, TimeSig};
use musiton_project::{
    AutoMode, AutoPoint, AutomationLane, ChannelStrip, Clip, Edit, ParamTarget, Project, Track,
    TrackKind,
};

use crate::clip_edit;
use crate::clip_view::{paint_clip, ClipPaint, TITLE_HEIGHT};
use crate::snap::Snap;
use crate::view::TimelineView;
use crate::UiAction;

/// Width of the track header column.
pub const HEADER_WIDTH: f32 = 220.0;
/// Height of one track row.
pub const ROW_HEIGHT: f32 = 56.0;
/// Height of the ruler.
pub const RULER_HEIGHT: f32 = 26.0;

/// Colours given to new tracks in turn.
pub const TRACK_COLORS: [[u8; 3]; 8] = [
    [231, 76, 60],
    [230, 126, 34],
    [241, 196, 15],
    [46, 204, 113],
    [26, 188, 156],
    [52, 152, 219],
    [155, 89, 182],
    [236, 112, 160],
];

/// One vertical grid line in the timeline.
#[derive(Clone, Copy)]
struct GridLine {
    x: f32,
    /// 0 = snap subdivision, 1 = beat, 2 = bar, 3 = labelled bar.
    level: u8,
    bar: i64,
}

/// Per-frame inputs of the arrangement view.
pub struct ArrangeInput<'a> {
    /// Song position in ticks.
    pub playhead: Option<f64>,
    /// Whether the transport runs.
    pub playing: bool,
    /// Waveform peaks of loaded audio files.
    pub peaks: &'a HashMap<AssetId, Arc<Peaks>>,
    /// What each track can automate: (target, name, parameter scale).
    pub auto_targets: &'a HashMap<TrackId, Vec<AutoTarget>>,
    /// Current RMS level (linear, left/right) per track, for the header meters.
    pub levels: &'a HashMap<TrackId, [f32; 2]>,
}

/// An automatable parameter: (target, display name, scale).
pub type AutoTarget = (ParamTarget, String, ParamInfo);

/// An automation point being dragged (previewed; committed on release).
#[derive(Debug, Clone)]
struct AutoDrag {
    track: TrackId,
    target: ParamTarget,
    /// Index of the dragged point, or of the segment's left point when bending.
    index: usize,
    bend: bool,
    origin: Pos2,
    orig: Vec<AutoPoint>,
}

/// What a clip drag changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragKind {
    Move,
    TrimStart,
    TrimEnd,
    FadeIn,
    FadeOut,
}

/// A clip drag in progress (previewed; committed as one edit on release).
#[derive(Debug)]
struct ClipDrag {
    kind: DragKind,
    origin: Pos2,
    /// Current pointer offset in ticks and rows.
    delta_ticks: f64,
    delta_rows: isize,
    /// (track index, clip) of every dragged clip as it was when the drag started.
    items: Vec<(usize, Clip)>,
}

/// State of the arrangement view.
#[derive(Debug, Default)]
pub struct Arrange {
    /// Zoom and horizontal scroll.
    pub view: TimelineView,
    /// Snap grid.
    pub snap: Snap,
    /// Scroll with the playhead while playing.
    pub follow: bool,
    /// Selected track.
    pub selected: Option<TrackId>,
    /// Selected clips.
    pub selected_clips: HashSet<ClipId>,
    renaming: Option<(TrackId, String)>,
    drag: Option<ClipDrag>,
    /// Actions produced inside popups (collected, returned at the end of the frame).
    pending: Vec<UiAction>,
    /// A MIDI clip to create (track, start, length) once the project is mutable again.
    pending_clip: Option<(TrackId, Tick, Tick)>,
    /// Show and edit automation instead of clips.
    pub automation_view: bool,
    /// The lane shown per track in automation view.
    auto_target: HashMap<TrackId, ParamTarget>,
    auto_drag: Option<AutoDrag>,
    /// Automatable parameters per track (copied from the input each frame).
    targets: HashMap<TrackId, Vec<AutoTarget>>,
    /// Rubber-band selection in progress: start point and the selection it adds to.
    box_select: Option<(Pos2, HashSet<ClipId>)>,
    /// Screen rectangles of the clips drawn this frame (for rubber-band selection).
    clip_rects: Vec<(ClipId, Rect)>,
    /// Folder tracks shown closed (their tracks are hidden).
    collapsed: HashSet<TrackId>,
}

impl Arrange {
    /// New view with follow on.
    pub fn new() -> Self {
        Self {
            follow: true,
            ..Self::default()
        }
    }

    /// Draws the view. `playhead` is the song position in ticks.
    /// Id allocation for new tracks happens directly on `project` (ids are never undone);
    /// all other changes come back as actions.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        project: &mut Project,
        input: &ArrangeInput,
    ) -> Vec<UiAction> {
        let ArrangeInput {
            playhead, playing, ..
        } = *input;
        let mut actions = Vec::new();
        self.targets.clone_from(input.auto_targets);
        if !crate::text_input_focused(ui.ctx())
            && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::A))
        {
            self.automation_view = !self.automation_view;
        }
        self.shortcuts(ui, project, playhead, &mut actions);
        self.toolbar(ui, project, &mut actions);

        let full = ui.available_rect_before_wrap();
        let lane_left = full.left() + HEADER_WIDTH;
        let lane_width = (full.width() - HEADER_WIDTH).max(1.0);

        // Wheel: Ctrl = zoom, horizontal (Shift) = scroll in time. Vertical is the scroll area's.
        if ui.rect_contains_pointer(full) {
            let (zoom, dx, pointer) = ui.input(|i| {
                (
                    i.zoom_delta(),
                    i.smooth_scroll_delta.x,
                    i.pointer.hover_pos(),
                )
            });
            if zoom != 1.0 {
                let anchor = pointer.map_or(0.0, |p| p.x - lane_left).max(0.0);
                self.view.zoom(zoom, anchor);
            }
            if dx != 0.0 {
                self.view.scroll_px(-dx);
            }
        }
        if playing && self.follow {
            if let Some(t) = playhead {
                self.view.follow(t, lane_width);
            }
        }

        let grid = self.grid(project.time_sig, lane_width);
        self.ruler(
            ui,
            project,
            &grid,
            lane_left,
            lane_width,
            playhead,
            &mut actions,
        );

        let preview = self.preview(project);
        let rows = self.rows(project);
        self.clip_rects.clear();
        let mut new_track_drop = None;
        egui::ScrollArea::vertical()
            .id_salt("tracks")
            .auto_shrink([false, false])
            .show_rows(ui, ROW_HEIGHT, rows.len(), |ui, range| {
                for r in range {
                    let Some(&(i, extra)) = rows.get(r) else {
                        continue;
                    };
                    let Some(track) = project.tracks.get(i) else {
                        continue;
                    };
                    let row = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), ROW_HEIGHT),
                        Sense::hover(),
                    );
                    let header =
                        Rect::from_min_size(row.0.min, egui::vec2(HEADER_WIDTH, ROW_HEIGHT));
                    let lane =
                        Rect::from_min_max(Pos2::new(header.right(), row.0.top()), row.0.max);
                    if let Some(target) = extra {
                        let rects = (header, lane);
                        self.extra_lane(
                            ui,
                            track,
                            target,
                            rects,
                            &grid,
                            playhead,
                            project,
                            &mut actions,
                        );
                        continue;
                    }
                    let indent = FOLDER_INDENT * project.depth(track.id) as f32;
                    let header = Rect::from_min_max(
                        Pos2::new(header.left() + indent, header.top()),
                        header.max,
                    );
                    self.header(ui, i, track, project, header, &mut actions);
                    if let Some(level) = input.levels.get(&track.id) {
                        header_meter(ui, header, *level);
                    }
                    let lane_input = LaneInput {
                        index: i,
                        grid: &grid,
                        playhead,
                        preview: &preview,
                        arrange: input,
                        project,
                    };
                    self.lane(ui, track, lane, &lane_input, &mut actions);
                }
                let rest = ui.available_rect_before_wrap();
                if rest.height() > 0.0 {
                    let r = ui.allocate_rect(rest, Sense::click());
                    if r.clicked() {
                        self.selected = None;
                        self.selected_clips.clear();
                    }
                    if let Some(path) = r.dnd_release_payload::<PathBuf>() {
                        new_track_drop = Some((vec![(*path).clone()], r.interact_pointer_pos()));
                    }
                    if r.contains_pointer() {
                        new_track_drop = new_track_drop.take().or_else(|| {
                            let files = dropped_files(ui);
                            (!files.is_empty())
                                .then(|| (files, ui.input(|i| i.pointer.latest_pos())))
                        });
                    }
                }
            });
        if let Some((files, pos)) = new_track_drop {
            let tick = pos.map_or(0.0, |p| self.view.tick(p.x - lane_left).max(0.0));
            actions.push(UiAction::Import {
                files,
                track: None,
                at: self.snap.round(tick, project.time_sig),
            });
        }
        self.update_box_select(ui);
        self.update_drag(ui, project, &mut actions);
        if let Some((track, start, length)) = self.pending_clip.take() {
            let clip = ClipId(project.alloc_id());
            actions.push(UiAction::Edit {
                edit: Edit::AddClip {
                    track,
                    clip: Some(Clip::Midi(musiton_project::MidiClip {
                        id: clip,
                        start,
                        length,
                        notes: Vec::new(),
                    })),
                },
                merge: false,
            });
            actions.push(UiAction::OpenEditor { track, clip });
            self.selected_clips = [clip].into_iter().collect();
        }
        actions.append(&mut self.pending);
        actions
    }

    /// Clips as they would be after the current drag: (track index, clip).
    fn preview(&self, project: &Project) -> Vec<(usize, Clip)> {
        let Some(d) = &self.drag else {
            return Vec::new();
        };
        let sig = project.time_sig;
        let last = project.tracks.len().saturating_sub(1) as isize;
        d.items
            .iter()
            .map(|(ti, c)| {
                let sr = f64::from(project.sample_rate);
                let tempo = &project.tempo;
                let at = c.start().0 as f64 + d.delta_ticks;
                let clip = match d.kind {
                    DragKind::Move => clip_edit::moved(c, d.delta_ticks, self.snap, sig),
                    DragKind::TrimStart => {
                        clip_edit::trimmed_start(c, d.delta_ticks, self.snap, sig, tempo, sr)
                    }
                    DragKind::TrimEnd => {
                        clip_edit::trimmed_end(c, d.delta_ticks, self.snap, sig, tempo, sr, None)
                    }
                    DragKind::FadeIn => {
                        let fade_end = match c {
                            Clip::Audio(a) => (a.start.0 + a.fade_in.0) as f64,
                            Clip::Midi(_) => at,
                        };
                        clip_edit::with_fade_in(c, fade_end + d.delta_ticks)
                    }
                    DragKind::FadeOut => {
                        let fade_start = match c {
                            Clip::Audio(a) => (a.start.0 + a.length.0 - a.fade_out.0) as f64,
                            Clip::Midi(_) => at,
                        };
                        clip_edit::with_fade_out(c, fade_start + d.delta_ticks)
                    }
                };
                let row = if d.kind == DragKind::Move {
                    (*ti as isize + d.delta_rows).clamp(0, last.max(0)) as usize
                } else {
                    *ti
                };
                // Clips only move between tracks of the same kind.
                let kind = |i: usize| project.tracks.get(i).map(|t| t.kind);
                let row = if kind(row) == kind(*ti) { row } else { *ti };
                (row, clip)
            })
            .collect()
    }

    /// Follows the pointer while a clip drag runs and commits it on release.
    fn update_drag(&mut self, ui: &Ui, project: &Project, actions: &mut Vec<UiAction>) {
        let Some(d) = &mut self.drag else {
            return;
        };
        let (pos, down) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.primary_down()));
        if let Some(p) = pos {
            d.delta_ticks = f64::from((p.x - d.origin.x) / self.view.px_per_tick);
            d.delta_rows = ((p.y - d.origin.y) / ROW_HEIGHT).round() as isize;
        }
        if down {
            return;
        }
        let preview = self.preview(project);
        let Some(d) = self.drag.take() else {
            return;
        };
        let mut edits = Vec::new();
        for ((from_i, old), (to_i, new)) in d.items.iter().zip(preview) {
            let (Some(from), Some(to)) = (project.tracks.get(*from_i), project.tracks.get(to_i))
            else {
                continue;
            };
            if from.id != to.id {
                edits.push(Edit::MoveClipToTrack {
                    clip: old.id(),
                    from: from.id,
                    to: to.id,
                    index: None,
                });
            }
            if new != *old {
                edits.push(Edit::ReplaceClip {
                    track: to.id,
                    clip: new,
                });
            }
        }
        if !edits.is_empty() {
            actions.push(UiAction::Edit {
                edit: Edit::Batch(edits),
                merge: false,
            });
        }
    }

    /// (track id, clip) of every selected clip.
    fn selected_clip_list(&self, project: &Project) -> Vec<(TrackId, Clip)> {
        project
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter().map(move |c| (t.id, c)))
            .filter(|(_, c)| self.selected_clips.contains(&c.id()))
            .map(|(t, c)| (t, c.clone()))
            .collect()
    }

    fn toolbar(&mut self, ui: &mut Ui, project: &mut Project, actions: &mut Vec<UiAction>) {
        ui.horizontal(|ui| {
            for (label, kind) in [
                ("+ Audio", TrackKind::Audio),
                ("+ Instrument", TrackKind::Instrument),
                ("+ Bus", TrackKind::Bus),
                ("+ Folder", TrackKind::Folder),
            ] {
                if ui.button(label).clicked() {
                    actions.push(self.add_track(project, kind));
                }
            }
            ui.separator();
            ui.label("Snap");
            egui::ComboBox::from_id_salt("snap")
                .width(60.0)
                .selected_text(self.snap.label())
                .show_ui(ui, |ui| {
                    for s in Snap::ALL {
                        ui.selectable_value(&mut self.snap, s, s.label());
                    }
                });
            ui.toggle_value(&mut self.automation_view, "Automation")
                .on_hover_text("Show and edit automation (A)");
            ui.toggle_value(&mut self.follow, "Follow")
                .on_hover_text("Scroll with the playhead");
            ui.separator();
            if ui
                .button("−")
                .on_hover_text("Zoom out (Ctrl+wheel)")
                .clicked()
            {
                self.view.zoom(0.5, 0.0);
            }
            if ui
                .button("+")
                .on_hover_text("Zoom in (Ctrl+wheel)")
                .clicked()
            {
                self.view.zoom(2.0, 0.0);
            }
        });
    }

    /// Builds the edit that adds a track of `kind` after the selected one (or at the end).
    fn add_track(&mut self, project: &mut Project, kind: TrackKind) -> UiAction {
        let id = TrackId(project.alloc_id());
        let n = project.tracks.iter().filter(|t| t.kind == kind).count() + 1;
        let base = match kind {
            TrackKind::Audio => "Audio",
            TrackKind::Instrument => "Instrument",
            TrackKind::Bus => "Bus",
            TrackKind::Folder => "Folder",
        };
        let index = self
            .selected
            .and_then(|s| project.track_index(s))
            .map_or(project.tracks.len(), |i| i + 1);
        // New tracks go into the selected folder (or the folder of the selected track).
        let parent = self.selected.and_then(|s| {
            let t = project.track(s)?;
            if t.kind == TrackKind::Folder {
                Some(t.id)
            } else {
                t.parent
            }
        });
        self.selected = Some(id);
        UiAction::Edit {
            edit: Edit::AddTrack {
                index,
                track: Some(Track {
                    id,
                    name: format!("{base} {n}"),
                    kind,
                    color: TRACK_COLORS[project.tracks.len() % TRACK_COLORS.len()],
                    parent,
                    ..Track::default()
                }),
            },
            merge: false,
        }
    }

    fn shortcuts(
        &mut self,
        ui: &mut Ui,
        project: &mut Project,
        playhead: Option<f64>,
        actions: &mut Vec<UiAction>,
    ) {
        if crate::text_input_focused(ui.ctx()) {
            return;
        }
        let shortcut =
            |m, k| ui.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(m, k)));
        if shortcut(Modifiers::COMMAND | Modifiers::SHIFT, Key::T) {
            actions.push(self.add_track(project, TrackKind::Instrument));
        } else if shortcut(Modifiers::COMMAND, Key::T) {
            actions.push(self.add_track(project, TrackKind::Audio));
        }
        let clips = self.selected_clip_list(project);
        if !clips.is_empty() {
            if shortcut(Modifiers::NONE, Key::Delete) {
                let edits = clips
                    .iter()
                    .map(|(t, c)| Edit::RemoveClip {
                        track: *t,
                        clip: c.id(),
                        removed: None,
                    })
                    .collect();
                actions.push(batch(edits));
                self.selected_clips.clear();
                return;
            }
            if shortcut(Modifiers::COMMAND, Key::D) {
                self.selected_clips.clear();
                let edits = clips
                    .into_iter()
                    .map(|(t, mut c)| {
                        let end = c.end();
                        let id = ClipId(project.alloc_id());
                        c.set_id(id);
                        c.set_start(end);
                        self.selected_clips.insert(id);
                        Edit::AddClip {
                            track: t,
                            clip: Some(c),
                        }
                    })
                    .collect();
                actions.push(batch(edits));
                return;
            }
            if shortcut(Modifiers::NONE, Key::S) {
                let at = Tick(playhead.unwrap_or(0.0).round() as i64);
                let sr = f64::from(project.sample_rate);
                let mut edits = Vec::new();
                for (t, c) in clips {
                    let id = ClipId(project.next_id);
                    if let Some((l, r)) = clip_edit::split(&c, at, id, &project.tempo, sr) {
                        project.alloc_id();
                        edits.push(Edit::ReplaceClip { track: t, clip: l });
                        edits.push(Edit::AddClip {
                            track: t,
                            clip: Some(r),
                        });
                    }
                }
                if !edits.is_empty() {
                    actions.push(batch(edits));
                }
                return;
            }
        }
        let Some(sel) = self.selected.filter(|s| project.track(*s).is_some()) else {
            return;
        };
        if shortcut(Modifiers::NONE, Key::Delete) {
            actions.push(UiAction::Edit {
                edit: Edit::RemoveTrack {
                    id: sel,
                    removed: None,
                },
                merge: false,
            });
            self.selected = None;
        }
        if shortcut(Modifiers::NONE, Key::F2) {
            if let Some(t) = project.track(sel) {
                self.renaming = Some((sel, t.name.clone()));
            }
        }
    }

    /// Grid lines visible in a lane `width` px wide.
    fn grid(&self, sig: TimeSig, width: f32) -> Vec<GridLine> {
        let bar_ticks = sig.ticks_per_bar();
        let beat_ticks = sig.ticks_per_beat();
        let bar_px = bar_ticks as f32 * self.view.px_per_tick;
        let beat_px = beat_ticks as f32 * self.view.px_per_tick;
        let label_step = self.view.bar_label_step(sig, 48.0);
        let snap_step = self
            .snap
            .step(sig)
            .filter(|s| *s < beat_ticks && *s as f32 * self.view.px_per_tick >= 14.0);
        let first_bar = (self.view.scroll / bar_ticks as f64).floor() as i64;
        let mut lines = Vec::new();
        let mut bar = first_bar;
        loop {
            let bar_tick = bar * bar_ticks;
            let x = self.view.x(bar_tick as f64);
            if x > width {
                break;
            }
            if bar % label_step == 0 {
                lines.push(GridLine { x, level: 3, bar });
            } else if bar_px >= 6.0 {
                lines.push(GridLine { x, level: 2, bar });
            }
            if beat_px >= 12.0 {
                for b in 0..i64::from(sig.num) {
                    let beat_tick = bar_tick + b * beat_ticks;
                    if b > 0 {
                        lines.push(GridLine {
                            x: self.view.x(beat_tick as f64),
                            level: 1,
                            bar,
                        });
                    }
                    if let Some(step) = snap_step {
                        let mut t = beat_tick + step;
                        while t < beat_tick + beat_ticks {
                            lines.push(GridLine {
                                x: self.view.x(t as f64),
                                level: 0,
                                bar,
                            });
                            t += step;
                        }
                    }
                }
            }
            bar += 1;
        }
        lines.retain(|l| l.x >= 0.0);
        lines
    }

    #[allow(clippy::too_many_arguments)]
    fn ruler(
        &mut self,
        ui: &mut Ui,
        project: &Project,
        grid: &[GridLine],
        lane_left: f32,
        lane_width: f32,
        playhead: Option<f64>,
        actions: &mut Vec<UiAction>,
    ) {
        let (row, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), RULER_HEIGHT),
            Sense::hover(),
        );
        let rect = Rect::from_min_size(
            Pos2::new(lane_left, row.top()),
            egui::vec2(lane_width, RULER_HEIGHT),
        );
        let resp = ui.interact(rect, ui.scope_id().with("ruler"), Sense::click_and_drag());
        if resp.clicked() || resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                let tick = self.view.tick(p.x - rect.left()).max(0.0);
                actions.push(UiAction::Locate(self.snap.round(tick, project.time_sig)));
            }
        }
        let painter = ui.painter_at(rect);
        let visuals = ui.visuals();
        painter.rect_filled(rect, 0.0, visuals.extreme_bg_color);

        if let Some((a, b)) = project.loop_range {
            let x0 = rect.left() + self.view.x(a.0 as f64);
            let x1 = rect.left() + self.view.x(b.0 as f64);
            let color = if project.loop_enabled {
                Color32::from_rgba_unmultiplied(80, 160, 255, 90)
            } else {
                Color32::from_rgba_unmultiplied(160, 160, 160, 40)
            };
            painter.rect_filled(
                Rect::from_x_y_ranges(x0..=x1, rect.top()..=rect.top() + 6.0),
                0.0,
                color,
            );
        }
        let text = visuals.text_color();
        let weak = visuals.weak_text_color();
        for l in grid {
            let x = rect.left() + l.x;
            let (h, c) = match l.level {
                3 => (RULER_HEIGHT, text),
                2 => (10.0, weak),
                1 => (6.0, weak),
                _ => continue,
            };
            painter.line_segment(
                [Pos2::new(x, rect.bottom() - h), Pos2::new(x, rect.bottom())],
                Stroke::new(1.0, c),
            );
            if l.level == 3 {
                painter.text(
                    Pos2::new(x + 3.0, rect.top() + 6.0),
                    Align2::LEFT_TOP,
                    (l.bar + 1).to_string(),
                    FontId::proportional(12.0),
                    text,
                );
            }
        }
        if let Some(t) = playhead {
            let x = rect.left() + self.view.x(t);
            if rect.x_range().contains(x) {
                let c = playhead_color();
                painter.add(egui::Shape::convex_polygon(
                    vec![
                        Pos2::new(x - 6.0, rect.top()),
                        Pos2::new(x + 6.0, rect.top()),
                        Pos2::new(x, rect.top() + 10.0),
                    ],
                    c,
                    Stroke::NONE,
                ));
                painter.line_segment(
                    [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                    Stroke::new(1.0, c),
                );
            }
        }
    }

    fn header(
        &mut self,
        ui: &mut Ui,
        index: usize,
        track: &Track,
        project: &Project,
        rect: Rect,
        actions: &mut Vec<UiAction>,
    ) {
        let id = ui.scope_id().with(("header", track.id));
        let resp = ui.interact(rect, id, Sense::click_and_drag());
        let selected = self.selected == Some(track.id);
        let visuals = ui.visuals();
        let bg = if selected {
            visuals.selection.bg_fill.gamma_multiply(0.5)
        } else {
            visuals.faint_bg_color
        };
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect.shrink2(egui::vec2(0.0, 1.0)), 2.0, bg);
        let [r, g, b] = track.color;
        painter.rect_filled(
            Rect::from_min_size(rect.min, egui::vec2(6.0, rect.height())),
            0.0,
            Color32::from_rgb(r, g, b),
        );

        // Drag & drop reordering.
        if resp.drag_started() {
            resp.dnd_set_drag_payload(index);
        }
        if let Some(from) = resp.dnd_hover_payload::<usize>() {
            if *from != index {
                let y = if *from < index {
                    rect.bottom()
                } else {
                    rect.top()
                };
                painter.hline(
                    rect.x_range(),
                    y,
                    Stroke::new(2.0, visuals.selection.stroke.color),
                );
            }
        }
        if let Some(fx) = resp.dnd_release_payload::<crate::device_panel::FxDrag>() {
            if fx.track != track.id && track.kind != TrackKind::Folder {
                actions.push(UiAction::MoveDevice {
                    from: fx.track,
                    device: fx.device,
                    to: track.id,
                });
            }
        }
        if resp
            .dnd_hover_payload::<crate::device_panel::FxDrag>()
            .is_some()
        {
            painter.rect_stroke(
                rect,
                2.0,
                Stroke::new(2.0, visuals.selection.stroke.color),
                egui::StrokeKind::Inside,
            );
        }
        if let Some(from) = resp.dnd_release_payload::<usize>() {
            if *from != index {
                actions.push(UiAction::Edit {
                    edit: Edit::MoveTrack {
                        from: *from,
                        to: index,
                    },
                    merge: false,
                });
            }
        }
        if resp.clicked() {
            self.selected = Some(track.id);
        }
        if resp.double_clicked() {
            self.renaming = Some((track.id, track.name.clone()));
        }
        resp.context_menu(|ui| self.header_menu(ui, project, track, actions));

        let inner = rect.shrink2(egui::vec2(12.0, 6.0));
        ui.scope_builder(UiBuilder::new().max_rect(inner), |ui| {
            ui.horizontal(|ui| {
                if track.kind == TrackKind::Folder {
                    let open = !self.collapsed.contains(&track.id);
                    let r = folder_toggle(ui, open).on_hover_text(if open {
                        "Close folder"
                    } else {
                        "Open folder"
                    });
                    if r.clicked() && !self.collapsed.remove(&track.id) {
                        self.collapsed.insert(track.id);
                    }
                }
                match &mut self.renaming {
                    Some((rid, text)) if *rid == track.id => {
                        let edit = ui.add(egui::TextEdit::singleline(text).desired_width(130.0));
                        edit.request_focus();
                        let enter = ui.input(|i| i.key_pressed(Key::Enter));
                        let escape = ui.input(|i| i.key_pressed(Key::Escape));
                        if escape {
                            self.renaming = None;
                        } else if edit.lost_focus() || enter {
                            let name = text.trim().to_owned();
                            if !name.is_empty() && name != track.name {
                                actions.push(UiAction::Edit {
                                    edit: Edit::RenameTrack { id: track.id, name },
                                    merge: false,
                                });
                            }
                            self.renaming = None;
                        }
                    }
                    _ => {
                        ui.add(
                            egui::Label::new(egui::RichText::new(&track.name).strong()).truncate(),
                        );
                    }
                }
            });
            if self.automation_view && track.kind != TrackKind::Folder {
                self.auto_header(ui, track, actions);
                return;
            }
            ui.horizontal(|ui| {
                ui.weak(kind_label(track.kind));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut strip = track.strip.clone();
                    let s = ui.selectable_label(strip.solo, "S").on_hover_text("Solo");
                    let m = ui.selectable_label(strip.mute, "M").on_hover_text("Mute");
                    if m.clicked() {
                        strip.mute = !strip.mute;
                    }
                    if s.clicked() {
                        strip.solo = !strip.solo;
                    }
                    if m.clicked() || s.clicked() {
                        actions.push(set_strip(track.id, strip));
                    }
                    if track.kind == TrackKind::Audio {
                        let armed = track.record.armed;
                        let text = egui::RichText::new("R").color(if armed {
                            Color32::from_rgb(255, 70, 70)
                        } else {
                            ui.visuals().text_color()
                        });
                        if ui
                            .selectable_label(armed, text)
                            .on_hover_text("Arm for recording")
                            .clicked()
                        {
                            let mut record = track.record;
                            record.armed = !armed;
                            actions.push(UiAction::Edit {
                                edit: Edit::SetRecord {
                                    id: track.id,
                                    record,
                                },
                                merge: false,
                            });
                        }
                    }
                });
            });
        });
    }

    fn header_menu(
        &mut self,
        ui: &mut Ui,
        project: &Project,
        track: &Track,
        actions: &mut Vec<UiAction>,
    ) {
        if ui.button("Rename (F2)").clicked() {
            self.renaming = Some((track.id, track.name.clone()));
            ui.close();
        }
        ui.horizontal(|ui| {
            ui.label("Colour");
            let mut color = track.color;
            if ui.color_edit_button_srgb(&mut color).changed() {
                actions.push(UiAction::Edit {
                    edit: Edit::SetTrackColor {
                        id: track.id,
                        color,
                    },
                    merge: true,
                });
            }
        });
        ui.horizontal_wrapped(|ui| {
            for c in TRACK_COLORS {
                let (r, swatch) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), Sense::click());
                ui.painter()
                    .rect_filled(r, 2.0, Color32::from_rgb(c[0], c[1], c[2]));
                if swatch.clicked() {
                    actions.push(UiAction::Edit {
                        edit: Edit::SetTrackColor {
                            id: track.id,
                            color: c,
                        },
                        merge: false,
                    });
                }
            }
        });
        ui.separator();
        self.folder_menu(ui, project, track, actions);
        if ui.button("Delete track (Del)").clicked() {
            actions.push(UiAction::Edit {
                edit: Edit::RemoveTrack {
                    id: track.id,
                    removed: None,
                },
                merge: false,
            });
            ui.close();
        }
    }

    fn lane(
        &mut self,
        ui: &mut Ui,
        track: &Track,
        rect: Rect,
        input: &LaneInput,
        actions: &mut Vec<UiAction>,
    ) {
        let sense = if self.automation_view {
            Sense::click()
        } else {
            Sense::click_and_drag()
        };
        let resp = ui.interact(rect, ui.scope_id().with(("lane", track.id)), sense);
        if resp.clicked() {
            self.selected = Some(track.id);
            self.selected_clips.clear();
        }
        if resp.drag_started() {
            if let Some(p) = resp.interact_pointer_pos() {
                let add = ui.input(|i| i.modifiers.shift || i.modifiers.command);
                let base = if add {
                    self.selected_clips.clone()
                } else {
                    HashSet::new()
                };
                self.box_select = Some((p, base));
            }
        }
        if resp.double_clicked() && track.kind == TrackKind::Instrument {
            if let Some(p) = resp.interact_pointer_pos() {
                let sig = input.project.time_sig;
                let bar = sig.ticks_per_bar();
                let start = Snap::Bar.floor(self.view.tick(p.x - rect.left()).max(0.0), sig);
                self.pending_clip = Some((track.id, start, Tick(bar)));
            }
        }
        let pointer_tick = |p: Pos2| {
            self.snap.round(
                self.view.tick(p.x - rect.left()).max(0.0),
                input.project.time_sig,
            )
        };
        if let Some(path) = resp.dnd_release_payload::<PathBuf>() {
            actions.push(UiAction::Import {
                files: vec![(*path).clone()],
                track: Some(track.id),
                at: resp.interact_pointer_pos().map_or(Tick(0), pointer_tick),
            });
        }
        if resp.contains_pointer() {
            let files = dropped_files(ui);
            if !files.is_empty() {
                let at = ui
                    .input(|i| i.pointer.latest_pos())
                    .map_or(Tick(0), pointer_tick);
                actions.push(UiAction::Import {
                    files,
                    track: Some(track.id),
                    at,
                });
            }
        }

        let painter = ui.painter_at(rect);
        let visuals = ui.visuals();
        let selected = self.selected == Some(track.id);
        let hovered_drop = resp.dnd_hover_payload::<PathBuf>().is_some()
            || (resp.contains_pointer() && ui.input(|i| !i.raw.hovered_files.is_empty()));
        let bg = if hovered_drop {
            visuals.selection.bg_fill.gamma_multiply(0.4)
        } else if selected {
            visuals.faint_bg_color.gamma_multiply(1.6)
        } else {
            visuals.extreme_bg_color
        };
        painter.rect_filled(rect.shrink2(egui::vec2(0.0, 1.0)), 0.0, bg);
        let base = visuals.weak_text_color();
        for l in input.grid {
            let alpha = match l.level {
                3 | 2 => 0.35,
                1 => 0.15,
                _ => 0.07,
            };
            let x = rect.left() + l.x;
            painter.vline(
                x,
                rect.y_range(),
                Stroke::new(1.0, base.gamma_multiply(alpha)),
            );
        }

        let dragged: HashSet<ClipId> = self
            .drag
            .iter()
            .flat_map(|d| d.items.iter().map(|(_, c)| c.id()))
            .collect();
        let clip_area = rect.shrink2(egui::vec2(0.0, 3.0));
        for clip in &track.clips {
            let crect = self.clip_rect(clip, clip_area);
            if crect.right() < rect.left() || crect.left() > rect.right() {
                continue;
            }
            if !dragged.contains(&clip.id()) {
                self.paint(&painter, clip, crect, rect, track, input);
            }
            self.clip_rects.push((clip.id(), crect.intersect(rect)));
            if !self.automation_view {
                self.clip_interaction(ui, input, clip, crect);
            }
        }
        for (_, clip) in input.preview.iter().filter(|(row, _)| *row == input.index) {
            let crect = self.clip_rect(clip, clip_area);
            self.paint(&painter, clip, crect, rect, track, input);
        }

        if self.automation_view && track.kind != TrackKind::Folder {
            let target = self.shown_target(track.id);
            self.auto_lane(ui, track, target, rect, input.project, actions);
        }
        if let Some(t) = input.playhead {
            let x = rect.left() + self.view.x(t);
            if rect.x_range().contains(x) {
                painter.vline(x, rect.y_range(), Stroke::new(1.0, playhead_color()));
            }
        }
    }

    fn clip_rect(&self, clip: &Clip, area: Rect) -> Rect {
        let x0 = area.left() + self.view.x(clip.start().0 as f64);
        let x1 = area.left() + self.view.x(clip.end().0 as f64);
        Rect::from_x_y_ranges(x0..=x1.max(x0 + 2.0), area.y_range())
    }

    fn paint(
        &self,
        painter: &egui::Painter,
        clip: &Clip,
        crect: Rect,
        lane: Rect,
        track: &Track,
        input: &LaneInput,
    ) {
        let (name, peaks) = match clip {
            Clip::Audio(a) => {
                let name = input
                    .project
                    .assets
                    .iter()
                    .find(|x| x.id == a.asset)
                    .and_then(|x| std::path::Path::new(&x.file).file_stem())
                    .map_or("audio".to_owned(), |s| s.to_string_lossy().into_owned());
                (name, input.arrange.peaks.get(&a.asset).map(|p| p.as_ref()))
            }
            Clip::Midi(_) => ("MIDI".to_owned(), None),
        };
        paint_clip(
            painter,
            &ClipPaint {
                clip,
                rect: crect,
                clip_to: lane,
                color: track.color,
                selected: self.selected_clips.contains(&clip.id()),
                name: &name,
                peaks,
                view: &self.view,
                lane_left: lane.left(),
                tempo: &input.project.tempo,
            },
        );
    }

    fn clip_interaction(&mut self, ui: &mut Ui, input: &LaneInput, clip: &Clip, crect: Rect) {
        let resp = ui.interact(
            crect,
            ui.scope_id().with(("clip", clip.id())),
            Sense::click_and_drag(),
        );
        let px = self.view.px_per_tick;
        let kind = |p: Pos2| {
            let edge = 6.0_f32.min(crect.width() / 4.0);
            let near_top =
                p.y < crect.top() + TITLE_HEIGHT + 5.0 && p.y > crect.top() + TITLE_HEIGHT - 5.0;
            if let Clip::Audio(a) = clip {
                let fi = crect.left() + (a.fade_in.0 as f32) * px;
                let fo = crect.right() - (a.fade_out.0 as f32) * px;
                if near_top && (p.x - fi).abs() < 6.0 {
                    return DragKind::FadeIn;
                }
                if near_top && (p.x - fo).abs() < 6.0 {
                    return DragKind::FadeOut;
                }
            }
            if p.x < crect.left() + edge {
                DragKind::TrimStart
            } else if p.x > crect.right() - edge {
                DragKind::TrimEnd
            } else {
                DragKind::Move
            }
        };
        if let Some(p) = resp.hover_pos() {
            match kind(p) {
                DragKind::TrimStart | DragKind::TrimEnd => {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                DragKind::FadeIn | DragKind::FadeOut => {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                DragKind::Move => {}
            }
        }
        let additive = ui.input(|i| i.modifiers.shift || i.modifiers.command);
        if resp.clicked() {
            self.select_clip(clip.id(), additive);
        }
        if let (Clip::Midi(_), Some(t)) = (clip, input.project.tracks.get(input.index)) {
            if resp.double_clicked() || resp.clicked() {
                self.pending.push(UiAction::OpenEditor {
                    track: t.id,
                    clip: clip.id(),
                });
            }
        }
        if resp.drag_started() {
            if let Some(origin) = resp.interact_pointer_pos() {
                if !self.selected_clips.contains(&clip.id()) {
                    self.select_clip(clip.id(), additive);
                }
                let kind = kind(origin);
                let items = if kind == DragKind::Move {
                    input
                        .project
                        .tracks
                        .iter()
                        .enumerate()
                        .flat_map(|(i, t)| t.clips.iter().map(move |c| (i, c)))
                        .filter(|(_, c)| self.selected_clips.contains(&c.id()))
                        .map(|(i, c)| (i, c.clone()))
                        .collect()
                } else {
                    vec![(input.index, clip.clone())]
                };
                self.drag = Some(ClipDrag {
                    kind,
                    origin,
                    delta_ticks: 0.0,
                    delta_rows: 0,
                    items,
                });
            }
        }
        if let Clip::Audio(a) = clip {
            let track_id = input.project.tracks.get(input.index).map(|t| t.id);
            resp.context_menu(|ui| {
                let mut gain = a.gain_db;
                ui.horizontal(|ui| {
                    ui.label("Clip gain");
                    let r = ui.add(
                        egui::DragValue::new(&mut gain)
                            .range(-60.0..=24.0)
                            .speed(0.1)
                            .suffix(" dB"),
                    );
                    if r.changed() {
                        if let Some(t) = track_id {
                            let mut c = a.clone();
                            c.gain_db = gain;
                            self.pending.push(UiAction::Edit {
                                edit: Edit::ReplaceClip {
                                    track: t,
                                    clip: Clip::Audio(c),
                                },
                                merge: !r.drag_started(),
                            });
                        }
                    }
                });
                let label = if a.muted { "Unmute clip" } else { "Mute clip" };
                if ui.button(label).clicked() {
                    if let Some(t) = track_id {
                        let mut c = a.clone();
                        c.muted = !c.muted;
                        self.pending.push(UiAction::Edit {
                            edit: Edit::ReplaceClip {
                                track: t,
                                clip: Clip::Audio(c),
                            },
                            merge: false,
                        });
                    }
                    ui.close();
                }
                ui.weak("Ctrl+D duplicate · S split at playhead · Del delete");
            });
        }
    }

    fn select_clip(&mut self, id: ClipId, additive: bool) {
        if additive {
            if !self.selected_clips.remove(&id) {
                self.selected_clips.insert(id);
            }
        } else {
            self.selected_clips.clear();
            self.selected_clips.insert(id);
        }
    }
}

/// Inputs for drawing one lane.
struct LaneInput<'a> {
    index: usize,
    grid: &'a [GridLine],
    playhead: Option<f64>,
    preview: &'a [(usize, Clip)],
    arrange: &'a ArrangeInput<'a>,
    project: &'a Project,
}

fn dropped_files(ui: &Ui) -> Vec<PathBuf> {
    ui.input(|i| {
        i.raw
            .dropped_files
            .iter()
            .map(|f| f.path().to_path_buf())
            .filter(|p| !p.as_os_str().is_empty())
            .collect()
    })
}

fn batch(edits: Vec<Edit>) -> UiAction {
    UiAction::Edit {
        edit: Edit::Batch(edits),
        merge: false,
    }
}

fn playhead_color() -> Color32 {
    Color32::from_rgb(255, 210, 80)
}

fn kind_label(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Audio => "Audio",
        TrackKind::Instrument => "Instrument",
        TrackKind::Bus => "Bus",
        TrackKind::Folder => "Folder",
    }
}

fn set_strip(id: TrackId, strip: ChannelStrip) -> UiAction {
    UiAction::Edit {
        edit: Edit::SetStrip {
            track: Some(id),
            strip,
        },
        merge: false,
    }
}

const AUTO_MODES: [(AutoMode, &str); 5] = [
    (AutoMode::Off, "Off"),
    (AutoMode::Read, "Read"),
    (AutoMode::Write, "Write"),
    (AutoMode::Touch, "Touch"),
    (AutoMode::Latch, "Latch"),
];

/// Point handle radius.
const POINT_R: f32 = 4.0;

impl Arrange {
    /// The target shown for `track` (default: volume).
    fn shown_target(&self, track: TrackId) -> ParamTarget {
        self.auto_target
            .get(&track)
            .copied()
            .unwrap_or(ParamTarget::Volume)
    }

    fn auto_edit(
        track: &Track,
        target: ParamTarget,
        change: impl FnOnce(&mut AutomationLane),
    ) -> UiAction {
        let mut lanes = track.automation.clone();
        let i = match lanes.iter().position(|l| l.target == target) {
            Some(i) => i,
            None => {
                lanes.push(AutomationLane {
                    target,
                    points: Vec::new(),
                    mode: AutoMode::Read,
                });
                lanes.len() - 1
            }
        };
        if let Some(l) = lanes.get_mut(i) {
            change(l);
            l.points.sort_by_key(|p| p.tick);
        }
        UiAction::Edit {
            edit: Edit::SetAutomation {
                track: track.id,
                lanes,
            },
            merge: false,
        }
    }

    /// Header row in automation view: parameter and mode.
    fn auto_header(&mut self, ui: &mut Ui, track: &Track, actions: &mut Vec<UiAction>) {
        let targets = self.targets.get(&track.id).cloned().unwrap_or_default();
        let mut target = self.shown_target(track.id);
        let lane = track.automation.iter().find(|l| l.target == target);
        ui.horizontal(|ui| {
            let name = targets
                .iter()
                .find(|t| t.0 == target)
                .map_or("?".to_owned(), |t| t.1.clone());
            egui::ComboBox::from_id_salt(("auto_target", track.id))
                .width(110.0)
                .selected_text(name)
                .show_ui(ui, |ui| {
                    for (t, n, _) in &targets {
                        let used = track
                            .automation
                            .iter()
                            .any(|l| l.target == *t && !l.points.is_empty());
                        let label = if used {
                            format!("● {n}").replace('●', "*")
                        } else {
                            n.clone()
                        };
                        ui.selectable_value(&mut target, *t, label);
                    }
                });
            self.auto_target.insert(track.id, target);
            let mut mode = lane.map_or(AutoMode::Read, |l| l.mode);
            let before = mode;
            egui::ComboBox::from_id_salt(("auto_mode", track.id))
                .width(54.0)
                .selected_text(AUTO_MODES.iter().find(|m| m.0 == mode).map_or("", |m| m.1))
                .show_ui(ui, |ui| {
                    for (m, n) in AUTO_MODES {
                        ui.selectable_value(&mut mode, m, n);
                    }
                });
            if mode != before {
                actions.push(Self::auto_edit(track, target, |l| l.mode = mode));
            }
        });
    }

    /// "Move into folder" / "Move out of folder" entries of the track menu.
    fn folder_menu(
        &mut self,
        ui: &mut Ui,
        project: &Project,
        track: &Track,
        actions: &mut Vec<UiAction>,
    ) {
        let folders: Vec<&Track> = project
            .tracks
            .iter()
            .filter(|f| {
                f.kind == TrackKind::Folder
                    && f.id != track.id
                    && Some(f.id) != track.parent
                    && !project.ancestors(f.id).any(|a| a == track.id)
            })
            .collect();
        if !folders.is_empty() {
            ui.menu_button("Move into folder", |ui| {
                for f in folders {
                    if ui.button(&f.name).clicked() {
                        actions.extend(move_into_folder(project, track.id, Some(f.id)));
                        ui.close();
                    }
                }
            });
        }
        if let Some(parent) = track.parent {
            if ui.button("Move out of folder").clicked() {
                let outer = project.track(parent).and_then(|p| p.parent);
                actions.extend(move_into_folder(project, track.id, outer));
                ui.close();
            }
        }
        ui.separator();
    }

    /// Rubber-band selection: selects the clips the rectangle touches while dragging.
    fn update_box_select(&mut self, ui: &Ui) {
        let Some((origin, base)) = &self.box_select else {
            return;
        };
        let (pos, down) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.primary_down()));
        let Some(pos) = pos else {
            return;
        };
        let area = Rect::from_two_pos(*origin, pos);
        let mut selected = base.clone();
        selected.extend(
            self.clip_rects
                .iter()
                .filter(|(_, r)| r.intersects(area))
                .map(|(id, _)| *id),
        );
        self.selected_clips = selected;
        if down {
            let painter = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::unique("box_select"),
            ));
            let color = ui.visuals().selection.bg_fill;
            painter.rect_filled(area, 0.0, color.gamma_multiply(0.15));
            painter.rect_stroke(area, 0.0, Stroke::new(1.0, color), egui::StrokeKind::Inside);
        } else {
            self.box_select = None;
        }
    }

    /// Rows of the track list: (track index, extra automation lane). In automation view
    /// every lane with points (other than the one shown over the clips) gets its own row
    /// under its track.
    fn rows(&self, project: &Project) -> Vec<(usize, Option<ParamTarget>)> {
        let mut rows = Vec::with_capacity(project.tracks.len());
        for (i, t) in project.tracks.iter().enumerate() {
            if project.ancestors(t.id).any(|f| self.collapsed.contains(&f)) {
                continue;
            }
            rows.push((i, None));
            if !self.automation_view || t.kind == TrackKind::Folder {
                continue;
            }
            let shown = self.shown_target(t.id);
            rows.extend(
                t.automation
                    .iter()
                    .filter(|l| l.target != shown && !l.points.is_empty())
                    .map(|l| (i, Some(l.target))),
            );
        }
        rows
    }

    /// An extra automation row: name and mode on the left, the curve on the right.
    #[allow(clippy::too_many_arguments)]
    fn extra_lane(
        &mut self,
        ui: &mut Ui,
        track: &Track,
        target: ParamTarget,
        (header, rect): (Rect, Rect),
        grid: &[GridLine],
        playhead: Option<f64>,
        project: &Project,
        actions: &mut Vec<UiAction>,
    ) {
        let visuals = ui.visuals().clone();
        let painter = ui.painter_at(header);
        painter.rect_filled(
            header.shrink2(egui::vec2(0.0, 1.0)),
            2.0,
            visuals.faint_bg_color,
        );
        let [r, g, b] = track.color;
        painter.rect_filled(
            Rect::from_min_size(header.min, egui::vec2(6.0, header.height())),
            0.0,
            Color32::from_rgb(r, g, b).gamma_multiply(0.5),
        );
        let name = self
            .targets
            .get(&track.id)
            .and_then(|v| v.iter().find(|t| t.0 == target))
            .map_or("?".to_owned(), |t| t.1.clone());
        let lane = track.automation.iter().find(|l| l.target == target);
        ui.scope_builder(
            UiBuilder::new().max_rect(header.shrink2(egui::vec2(16.0, 6.0))),
            |ui| {
                ui.add(egui::Label::new(egui::RichText::new(name).small()).truncate());
                ui.horizontal(|ui| {
                    let mut mode = lane.map_or(AutoMode::Read, |l| l.mode);
                    let before = mode;
                    egui::ComboBox::from_id_salt(("auto_mode", track.id, target))
                        .width(54.0)
                        .selected_text(AUTO_MODES.iter().find(|m| m.0 == mode).map_or("", |m| m.1))
                        .show_ui(ui, |ui| {
                            for (m, n) in AUTO_MODES {
                                ui.selectable_value(&mut mode, m, n);
                            }
                        });
                    if mode != before {
                        actions.push(Self::auto_edit(track, target, |l| l.mode = mode));
                    }
                    if ui
                        .small_button("Show")
                        .on_hover_text("Show this lane over the clips")
                        .clicked()
                    {
                        self.auto_target.insert(track.id, target);
                    }
                });
            },
        );
        let painter = ui.painter_at(rect);
        painter.rect_filled(
            rect.shrink2(egui::vec2(0.0, 1.0)),
            0.0,
            visuals.extreme_bg_color,
        );
        let base = visuals.weak_text_color();
        for l in grid.iter().filter(|l| l.level >= 2) {
            painter.vline(
                rect.left() + l.x,
                rect.y_range(),
                Stroke::new(1.0, base.gamma_multiply(0.25)),
            );
        }
        self.auto_lane(ui, track, target, rect, project, actions);
        if let Some(t) = playhead {
            let x = rect.left() + self.view.x(t);
            if rect.x_range().contains(x) {
                ui.painter_at(rect)
                    .vline(x, rect.y_range(), Stroke::new(1.0, playhead_color()));
            }
        }
    }

    /// Automation curve over a track lane, with point editing.
    fn auto_lane(
        &mut self,
        ui: &mut Ui,
        track: &Track,
        target: ParamTarget,
        rect: Rect,
        project: &Project,
        actions: &mut Vec<UiAction>,
    ) {
        let info = self
            .targets
            .get(&track.id)
            .and_then(|v| v.iter().find(|t| t.0 == target))
            .map(|t| t.2);
        let stored = track.automation.iter().find(|l| l.target == target);
        let dragging = self
            .auto_drag
            .clone()
            .filter(|d| d.track == track.id && d.target == target);
        let mut points = stored.map(|l| l.points.clone()).unwrap_or_default();
        let (pointer, down) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.primary_down()));
        let area = rect.shrink2(egui::vec2(0.0, 6.0));
        let y_of = |v: f32| area.bottom() - v.clamp(0.0, 1.0) * area.height();
        let v_of = |y: f32| ((area.bottom() - y) / area.height()).clamp(0.0, 1.0);
        let x_of = |t: i64| rect.left() + self.view.x(t as f64);
        let sig = project.time_sig;

        // Drag preview.
        if let (Some(d), Some(p)) = (dragging.as_ref(), pointer) {
            points = d.orig.clone();
            if d.bend {
                let dv = (d.origin.y - p.y) / area.height();
                if let Some(pt) = points.get_mut(d.index) {
                    pt.curve = (pt.curve + dv * 2.0).clamp(-1.0, 1.0);
                }
            } else if let Some(pt) = points.get_mut(d.index) {
                let tick = self.view.tick(p.x - rect.left()).max(0.0);
                pt.tick = self.snap.round(tick, sig);
                pt.value = v_of(p.y);
            }
            points.sort_by_key(|p| p.tick);
        }

        // Draw: dim the clips, then the curve and the points.
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_black_alpha(110));
        let color = Color32::from_rgb(255, 170, 60);
        let lane = AutomationLane {
            target,
            points: points.clone(),
            mode: stored.map_or(AutoMode::Read, |l| l.mode),
        };
        if !points.is_empty() {
            let mut line = Vec::new();
            let mut x = rect.left();
            while x <= rect.right() {
                let tick = self.view.tick(x - rect.left());
                if let Some(v) = lane.value_at(tick) {
                    line.push(Pos2::new(x, y_of(v)));
                }
                x += 3.0;
            }
            painter.add(egui::Shape::line(line, Stroke::new(1.5, color)));
            for p in &points {
                let c = Pos2::new(x_of(p.tick.0), y_of(p.value));
                if rect.x_range().contains(c.x) {
                    painter.circle_filled(c, POINT_R, color);
                }
            }
        } else {
            painter.text(
                Pos2::new(rect.left() + 8.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                "Click to add automation points",
                egui::FontId::proportional(11.0),
                Color32::from_gray(150),
            );
        }
        if lane.mode == AutoMode::Off {
            painter.rect_filled(rect, 0.0, Color32::from_black_alpha(80));
        }

        // Hover value.
        let resp = ui.interact(
            rect,
            ui.scope_id().with(("auto", track.id, target)),
            Sense::click_and_drag(),
        );
        let hit = |p: Pos2| {
            points.iter().position(|pt| {
                (x_of(pt.tick.0) - p.x).abs() <= POINT_R + 2.0
                    && (y_of(pt.value) - p.y).abs() <= POINT_R + 2.0
            })
        };
        if let (Some(p), Some(info)) = (resp.hover_pos(), info) {
            let v = lane
                .value_at(self.view.tick(p.x - rect.left()))
                .unwrap_or(v_of(p.y));
            resp.clone().on_hover_text(format!(
                "{}\nClick: add point · drag: move · right-click: delete · Alt+drag: bend",
                info.format(info.from_normalized(v))
            ));
        }
        if resp.secondary_clicked() {
            if let Some(i) = resp.interact_pointer_pos().and_then(hit) {
                actions.push(Self::auto_edit(track, target, |l| {
                    if i < l.points.len() {
                        l.points.remove(i);
                    }
                }));
            }
        }
        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos().filter(|p| hit(*p).is_none()) {
                let tick = self
                    .snap
                    .round(self.view.tick(p.x - rect.left()).max(0.0), sig);
                let value = v_of(p.y);
                actions.push(Self::auto_edit(track, target, |l| {
                    l.points.retain(|pt| pt.tick != tick);
                    l.points.push(AutoPoint {
                        tick,
                        value,
                        curve: 0.0,
                    });
                }));
            }
        }
        if resp.drag_started() {
            if let Some(p) = resp.interact_pointer_pos() {
                let alt = ui.input(|i| i.modifiers.alt);
                let start = match hit(p) {
                    Some(i) if !alt => Some((i, false)),
                    _ if alt => {
                        let tick = self.view.tick(p.x - rect.left());
                        let i = points.partition_point(|pt| (pt.tick.0 as f64) <= tick);
                        (i > 0 && i < points.len()).then(|| (i - 1, true))
                    }
                    _ => None,
                };
                if let Some((index, bend)) = start {
                    self.auto_drag = Some(AutoDrag {
                        track: track.id,
                        target,
                        index,
                        bend,
                        origin: p,
                        orig: points.clone(),
                    });
                }
            }
        }
        if dragging.is_some() && !down {
            self.auto_drag = None;
            actions.push(Self::auto_edit(track, target, |l| l.points = points));
        }
    }
}

/// Two thin level bars (left/right RMS, −60…0 dB) at the right edge of a track header.
fn header_meter(ui: &Ui, header: Rect, level: [f32; 2]) {
    let painter = ui.painter_at(header);
    let h = header.height() - 12.0;
    for (k, v) in level.into_iter().enumerate() {
        let db = musiton_core::units::gain_to_db(v);
        let x = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
        let left = header.right() - 9.0 + k as f32 * 4.0;
        let bar = Rect::from_min_max(
            Pos2::new(left, header.bottom() - 6.0 - h * x),
            Pos2::new(left + 3.0, header.bottom() - 6.0),
        );
        let color = if db > -3.0 {
            Color32::from_rgb(240, 80, 60)
        } else if db > -12.0 {
            Color32::from_rgb(230, 200, 60)
        } else {
            Color32::from_rgb(90, 200, 110)
        };
        painter.rect_filled(bar, 0.0, color);
    }
}

/// Open/close triangle of a folder header.
fn folder_toggle(ui: &mut Ui, open: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), Sense::click());
    let c = rect.center();
    let color = if resp.hovered() {
        ui.visuals().strong_text_color()
    } else {
        ui.visuals().text_color()
    };
    let pts = if open {
        vec![
            c + egui::vec2(-5.0, -3.0),
            c + egui::vec2(5.0, -3.0),
            c + egui::vec2(0.0, 4.0),
        ]
    } else {
        vec![
            c + egui::vec2(-3.0, -5.0),
            c + egui::vec2(4.0, 0.0),
            c + egui::vec2(-3.0, 5.0),
        ]
    };
    ui.painter()
        .add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
    resp
}

/// Header indentation per folder level.
const FOLDER_INDENT: f32 = 14.0;

/// One undo step that moves `id` into `folder` (`None` = top level) and places it after
/// the folder's last track, so folder contents stay together.
fn move_into_folder(project: &Project, id: TrackId, folder: Option<TrackId>) -> Option<UiAction> {
    let from = project.track_index(id)?;
    let mut edits = Vec::new();
    if let Some(f) = folder {
        let end = project
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.id != id && (t.id == f || project.ancestors(t.id).any(|a| a == f)))
            .map(|(i, _)| i)
            .max()?;
        let to = if from < end { end } else { end + 1 };
        if to != from {
            edits.push(Edit::MoveTrack { from, to });
        }
    }
    edits.push(Edit::SetParent { id, parent: folder });
    Some(UiAction::Edit {
        edit: Edit::Batch(edits),
        merge: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use musiton_project::UndoStack;

    fn project() -> Project {
        let mut p = Project::default();
        for (id, kind) in [
            (1, TrackKind::Audio),
            (2, TrackKind::Folder),
            (3, TrackKind::Audio),
            (4, TrackKind::Audio),
        ] {
            p.tracks.push(Track {
                id: TrackId(id),
                kind,
                ..Track::default()
            });
        }
        p.tracks[2].parent = Some(TrackId(2));
        p
    }

    fn apply(p: &mut Project, a: Option<UiAction>) {
        let Some(UiAction::Edit { edit, .. }) = a else {
            panic!("no edit");
        };
        UndoStack::default().perform(p, edit, false).unwrap();
    }

    fn order(p: &Project) -> Vec<(u64, Option<u64>)> {
        p.tracks
            .iter()
            .map(|t| (t.id.0, t.parent.map(|x| x.0)))
            .collect()
    }

    #[test]
    fn moving_into_a_folder_places_the_track_after_its_contents() {
        // Track 1 (above the folder) moves in after track 3.
        let mut p = project();
        let a = move_into_folder(&p, TrackId(1), Some(TrackId(2)));
        apply(&mut p, a);
        assert_eq!(
            order(&p),
            [(2, None), (3, Some(2)), (1, Some(2)), (4, None)]
        );
        // Track 4 (below) moves in at the end of the folder (it already is).
        let mut p = project();
        let a = move_into_folder(&p, TrackId(4), Some(TrackId(2)));
        apply(&mut p, a);
        assert_eq!(
            order(&p),
            [(1, None), (2, None), (3, Some(2)), (4, Some(2))]
        );
        // Moving out keeps the position.
        let a = move_into_folder(&p, TrackId(3), None);
        apply(&mut p, a);
        assert_eq!(order(&p), [(1, None), (2, None), (3, None), (4, Some(2))]);
    }

    #[test]
    fn closed_folders_hide_their_tracks() {
        let p = project();
        let mut a = Arrange::new();
        assert_eq!(a.rows(&p).len(), 4);
        a.collapsed.insert(TrackId(2));
        let shown: Vec<usize> = a.rows(&p).into_iter().map(|(i, _)| i).collect();
        assert_eq!(shown, [0, 1, 3]);
    }
}
