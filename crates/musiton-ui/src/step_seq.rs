//! Step sequencer: a grid of rows (keys or drum pads) × steps over one MIDI clip.
//!
//! It edits the clip's ordinary notes: a lit cell is a note that starts inside that step.
//! So playback, export, undo and the piano roll all see the same data.

use std::collections::HashMap;

use egui::{Color32, Pos2, Rect, Sense, Stroke, Ui};
use musiton_core::ids::{ClipId, TrackId};
use musiton_core::{Tick, PPQ};
use musiton_project::{Clip, Edit, MidiClip, Note, Project};

use crate::device_panel::key_name;
use crate::UiAction;

/// Velocity of a normal step.
pub const NORMAL_VELOCITY: u8 = 100;
/// Velocity of an accented step (Shift+click).
pub const ACCENT_VELOCITY: u8 = 127;

const ROW_HEIGHT: f32 = 20.0;
const LABEL_WIDTH: f32 = 110.0;
const CELL_WIDTH: f32 = 22.0;

/// Step length choices: (label, ticks).
pub const RESOLUTIONS: [(&str, i64); 3] = [("1/8", PPQ / 2), ("1/16", PPQ / 4), ("1/32", PPQ / 8)];

/// Index of the note of `key` that starts inside the step `start..start + len`.
pub fn note_in_step(notes: &[Note], key: u8, start: i64, len: i64) -> Option<usize> {
    notes
        .iter()
        .position(|n| n.pitch == key && (start..start + len).contains(&n.start.0))
}

/// Turns the step of `key` at `start` on (adds a one-step note with `velocity`) or off
/// (removes every note of `key` starting in it). Returns whether anything changed.
pub fn set_step(
    notes: &mut Vec<Note>,
    key: u8,
    start: i64,
    len: i64,
    on: bool,
    velocity: u8,
) -> bool {
    let present = note_in_step(notes, key, start, len).is_some();
    if on == present {
        return false;
    }
    if on {
        notes.push(Note {
            pitch: key,
            velocity,
            start: Tick(start),
            length: Tick(len),
            channel: 0,
        });
        notes.sort_by_key(|n| (n.start, n.pitch));
    } else {
        notes.retain(|n| !(n.pitch == key && (start..start + len).contains(&n.start.0)));
    }
    true
}

/// Switches a lit step between normal and accented velocity.
pub fn toggle_accent(notes: &mut [Note], key: u8, start: i64, len: i64) -> bool {
    let Some(i) = note_in_step(notes, key, start, len) else {
        return false;
    };
    let Some(n) = notes.get_mut(i) else {
        return false;
    };
    n.velocity = if n.velocity >= ACCENT_VELOCITY {
        NORMAL_VELOCITY
    } else {
        ACCENT_VELOCITY
    };
    true
}

/// A stroke across the grid: every cell it touches gets the same state.
#[derive(Debug, Clone)]
struct Paint {
    on: bool,
    notes: Vec<Note>,
}

/// Step sequencer state.
#[derive(Debug)]
pub struct StepSequencer {
    /// Step length in ticks.
    pub step: i64,
    /// Lowest key shown for melodic instruments.
    pub base_key: u8,
    stroke: Option<Paint>,
    /// The clip shown last (a newly opened clip scrolls to its notes).
    shown: Option<(TrackId, ClipId)>,
}

impl Default for StepSequencer {
    fn default() -> Self {
        Self {
            step: PPQ / 4,
            base_key: 48,
            stroke: None,
            shown: None,
        }
    }
}

/// What the sequencer needs from the app besides the project.
pub struct StepInput<'a> {
    /// The clip being edited (the one open in the piano roll).
    pub clip: Option<(TrackId, ClipId)>,
    /// Song position in ticks.
    pub playhead: Option<f64>,
    /// Drum pads (key, name) when the track's instrument is in drum mode.
    pub pads: Option<&'a [(u8, String)]>,
}

impl StepSequencer {
    /// Draws the sequencer for the open clip.
    pub fn ui(&mut self, ui: &mut Ui, project: &Project, input: &StepInput) -> Vec<UiAction> {
        let mut actions = Vec::new();
        let found = input.clip.and_then(|(t, c)| {
            match project.track(t)?.clips.iter().find(|x| x.id() == c)? {
                Clip::Midi(m) => Some((t, m.clone())),
                Clip::Audio(_) => None,
            }
        });
        let Some((track, clip)) = found else {
            ui.weak(
                "Open a MIDI clip (double-click it) to edit it as steps. \
                 Double-click an instrument track to create one.",
            );
            return actions;
        };

        if self.shown != input.clip {
            self.shown = input.clip;
            self.stroke = None;
            if let Some(lowest) = clip.notes.iter().map(|n| n.pitch).min() {
                // Start the two visible octaves at the C below the lowest note.
                self.base_key = (lowest - lowest % 12).min(104);
            }
        }
        let rows: Vec<(u8, String)> = match input.pads {
            Some(pads) => pads.iter().rev().cloned().collect(),
            None => (0..24u8)
                .rev()
                .filter_map(|i| self.base_key.checked_add(i))
                .filter(|k| *k <= 127)
                .map(|k| (k, key_name(k)))
                .collect(),
        };
        self.toolbar(ui, input.pads.is_some());

        let steps = (clip.length.0 / self.step).max(1);
        let notes = self
            .stroke
            .as_ref()
            .map_or(&clip.notes, |s| &s.notes)
            .clone();
        // Lit cells: (key, step) → velocity.
        let mut lit: HashMap<(u8, i64), u8> = HashMap::new();
        for n in &notes {
            if (0..clip.length.0).contains(&n.start.0) {
                lit.entry((n.pitch, n.start.0 / self.step))
                    .or_insert(n.velocity);
            }
        }
        let playing_step = input
            .playhead
            .map(|t| t - clip.start.0 as f64)
            .filter(|t| *t >= 0.0 && *t < clip.length.0 as f64)
            .map(|t| (t as i64) / self.step);

        let width = LABEL_WIDTH + steps as f32 * CELL_WIDTH;
        let height = rows.len() as f32 * ROW_HEIGHT;
        egui::ScrollArea::both()
            .id_salt("steps")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let (area, resp) =
                    ui.allocate_exact_size(egui::vec2(width, height), Sense::click_and_drag());
                let painter = ui.painter_at(area);
                let steps_per_beat = (PPQ / self.step).max(1);
                let cell = |row: usize, step: i64| {
                    Rect::from_min_size(
                        Pos2::new(
                            area.left() + LABEL_WIDTH + step as f32 * CELL_WIDTH,
                            area.top() + row as f32 * ROW_HEIGHT,
                        ),
                        egui::vec2(CELL_WIDTH, ROW_HEIGHT),
                    )
                    .shrink(1.5)
                };
                for (row, (key, name)) in rows.iter().enumerate() {
                    let y = area.top() + row as f32 * ROW_HEIGHT;
                    painter.text(
                        Pos2::new(area.left() + 6.0, y + ROW_HEIGHT / 2.0),
                        egui::Align2::LEFT_CENTER,
                        name,
                        egui::FontId::proportional(12.0),
                        ui.visuals().text_color(),
                    );
                    for step in 0..steps {
                        let r = cell(row, step);
                        if r.right() < ui.clip_rect().left() || r.left() > ui.clip_rect().right() {
                            continue;
                        }
                        let beat = (step / steps_per_beat) % 2 == 0;
                        let mut fill = if beat {
                            Color32::from_gray(42)
                        } else {
                            Color32::from_gray(32)
                        };
                        if let Some(v) = lit.get(&(*key, step)) {
                            fill = if *v >= ACCENT_VELOCITY {
                                Color32::from_rgb(255, 170, 70)
                            } else {
                                Color32::from_rgb(90, 170, 255)
                            };
                        }
                        painter.rect_filled(r, 2.0, fill);
                        if playing_step == Some(step) {
                            painter.rect_stroke(
                                r,
                                2.0,
                                Stroke::new(1.5, Color32::WHITE),
                                egui::StrokeKind::Inside,
                            );
                        }
                    }
                }
                // Bar lines.
                let per_bar = (project.time_sig.ticks_per_bar() / self.step).max(1);
                for step in (0..=steps).step_by(per_bar as usize) {
                    let x = area.left() + LABEL_WIDTH + step as f32 * CELL_WIDTH;
                    painter.vline(x, area.y_range(), Stroke::new(1.0, Color32::from_gray(90)));
                }

                let hit = |p: Pos2| {
                    let col = ((p.x - area.left() - LABEL_WIDTH) / CELL_WIDTH).floor();
                    let row = ((p.y - area.top()) / ROW_HEIGHT).floor();
                    if col < 0.0 || row < 0.0 || col as i64 >= steps {
                        return None;
                    }
                    let (key, _) = rows.get(row as usize)?;
                    Some((*key, col as i64 * self.step))
                };
                let shift = ui.input(|i| i.modifiers.shift);
                if resp.clicked() && shift {
                    if let Some((key, start)) = resp.interact_pointer_pos().and_then(hit) {
                        let mut v = clip.notes.clone();
                        if toggle_accent(&mut v, key, start, self.step) {
                            actions.push(commit(track, &clip, v));
                        }
                    }
                    return;
                }
                let pointer = ui.input(|i| i.pointer.latest_pos());
                if resp.is_pointer_button_down_on() && !shift {
                    if let Some((key, start)) = pointer.and_then(hit) {
                        let step = self.step;
                        let stroke = self.stroke.get_or_insert_with(|| Paint {
                            on: note_in_step(&clip.notes, key, start, step).is_none(),
                            notes: clip.notes.clone(),
                        });
                        set_step(
                            &mut stroke.notes,
                            key,
                            start,
                            step,
                            stroke.on,
                            NORMAL_VELOCITY,
                        );
                    }
                } else if let Some(s) = self.stroke.take() {
                    if s.notes != clip.notes {
                        actions.push(commit(track, &clip, s.notes));
                    }
                }
                if resp.hovered() {
                    resp.on_hover_text(
                        "Click or drag: steps on/off · Shift+click: accent · \
                         the clip's length sets the number of steps",
                    );
                }
            });
        actions
    }

    fn toolbar(&mut self, ui: &mut Ui, drums: bool) {
        ui.horizontal(|ui| {
            ui.label("Step");
            for (label, ticks) in RESOLUTIONS {
                ui.selectable_value(&mut self.step, ticks, label);
            }
            if !drums {
                ui.separator();
                ui.label("Lowest key");
                let mut key = self.base_key;
                ui.add(
                    egui::DragValue::new(&mut key)
                        .range(0..=104)
                        .custom_formatter(|v, _| key_name(v as u8)),
                )
                .on_hover_text("Drag to show other keys (two octaves are shown)");
                self.base_key = key;
            } else {
                ui.weak("Drum mode: one row per pad");
            }
        });
        ui.separator();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_turn_on_and_off_and_accent() {
        let step = PPQ / 4;
        let mut notes = Vec::new();
        assert!(set_step(
            &mut notes,
            36,
            2 * step,
            step,
            true,
            NORMAL_VELOCITY
        ));
        assert!(!set_step(
            &mut notes,
            36,
            2 * step,
            step,
            true,
            NORMAL_VELOCITY
        ));
        assert_eq!(notes.len(), 1);
        assert_eq!((notes[0].start.0, notes[0].length.0), (2 * step, step));
        // A note recorded slightly late still belongs to its step.
        notes.push(Note {
            pitch: 38,
            velocity: 90,
            start: Tick(step + 7),
            length: Tick(100),
            channel: 0,
        });
        assert_eq!(note_in_step(&notes, 38, step, step), Some(1));
        assert!(toggle_accent(&mut notes, 36, 2 * step, step));
        assert_eq!(notes[0].velocity, ACCENT_VELOCITY);
        assert!(toggle_accent(&mut notes, 36, 2 * step, step));
        assert_eq!(notes[0].velocity, NORMAL_VELOCITY);
        assert!(set_step(&mut notes, 38, step, step, false, 0));
        assert!(set_step(&mut notes, 36, 2 * step, step, false, 0));
        assert!(notes.is_empty());
        assert!(!toggle_accent(&mut notes, 36, 0, step));
    }
}
