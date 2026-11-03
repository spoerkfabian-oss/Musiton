//! Device panel: instrument choice, presets, parameter knobs, sampler samples.

use egui::{Color32, Pos2, Sense, Stroke, Ui};
use musiton_core::ids::{AssetId, DeviceId, TrackId};
use musiton_core::params::{ParamInfo, ParamKind};
use musiton_project::{DeviceKind, DeviceSlot, Edit, Project, TrackKind};

use crate::UiAction;

/// Lowest pad key in drum mode (C1).
pub const FIRST_PAD: u8 = 36;
/// Number of drum pads shown.
pub const PADS: u8 = 16;

/// A device type the panel can offer.
#[derive(Debug, Clone)]
pub struct DeviceType {
    /// Stable id.
    pub type_id: &'static str,
    /// Display name.
    pub name: &'static str,
    /// Parameters.
    pub params: &'static [ParamInfo],
    /// Plays samples (shows the sample section).
    pub uses_samples: bool,
    /// Presets: (name, values) — factory presets first, then the user's.
    pub presets: Vec<(String, Vec<(u64, f32)>)>,
    /// Id of the "drum mode" parameter, if any.
    pub drum_param: Option<u64>,
    /// Id of the "root key" parameter, if any.
    pub root_param: Option<u64>,
}

/// Panel state.
#[derive(Debug, Default)]
pub struct DevicePanel {
    preset_name: String,
}

/// Draws a knob for `info`; returns the response and whether the value changed.
pub fn knob(ui: &mut Ui, info: &ParamInfo, value: &mut f32) -> (egui::Response, bool) {
    let size = egui::vec2(64.0, 70.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let mut changed = false;
    if resp.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        let dy = resp.drag_delta().y;
        let x = info.to_normalized(*value) - dy * if fine { 0.001 } else { 0.005 };
        let v = info.from_normalized(x);
        changed = v != *value;
        *value = v;
    }
    if resp.double_clicked() && *value != info.default {
        *value = info.default;
        changed = true;
    }
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    let center = Pos2::new(rect.center().x, rect.top() + 28.0);
    let r = 18.0;
    let arc = |from: f32, to: f32, color: Color32, painter: &egui::Painter| {
        let steps = 24;
        let pts: Vec<Pos2> = (0..=steps)
            .map(|i| {
                let a = from + (to - from) * i as f32 / steps as f32;
                center + egui::vec2(a.cos(), a.sin()) * r
            })
            .collect();
        painter.add(egui::Shape::line(pts, Stroke::new(3.0, color)));
    };
    let start = std::f32::consts::PI * 0.75;
    let span = std::f32::consts::PI * 1.5;
    arc(
        start,
        start + span,
        visuals.widgets.inactive.bg_fill,
        &painter,
    );
    let x = info.to_normalized(*value);
    let accent = if resp.hovered() || resp.dragged() {
        Color32::from_rgb(255, 210, 80)
    } else {
        Color32::from_rgb(90, 170, 255)
    };
    arc(start, start + span * x, accent, &painter);
    let a = start + span * x;
    painter.line_segment(
        [center, center + egui::vec2(a.cos(), a.sin()) * (r - 4.0)],
        Stroke::new(2.0, visuals.text_color()),
    );
    painter.text(
        Pos2::new(rect.center().x, rect.top()),
        egui::Align2::CENTER_TOP,
        info.name,
        egui::FontId::proportional(10.0),
        visuals.text_color(),
    );
    painter.text(
        Pos2::new(rect.center().x, rect.bottom()),
        egui::Align2::CENTER_BOTTOM,
        info.format(*value),
        egui::FontId::monospace(10.0),
        visuals.weak_text_color(),
    );
    let resp = resp.on_hover_text("Drag up/down (Shift = fine), double-click = default");
    (resp, changed)
}

impl DevicePanel {
    /// Draws the panel for `track`. `asset_name` names sample files.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        project: &Project,
        track: Option<TrackId>,
        types: &[DeviceType],
        asset_name: &dyn Fn(AssetId) -> String,
    ) -> Vec<UiAction> {
        let mut actions = Vec::new();
        let Some(t) = track
            .and_then(|id| project.track(id))
            .filter(|t| t.kind == TrackKind::Instrument)
        else {
            ui.weak("Select an instrument track to edit its instrument.");
            return actions;
        };
        let slot = t.devices.first().cloned();
        let current = slot
            .as_ref()
            .map_or(types.first().map_or("", |d| d.type_id), |s| s.type_id());
        let Some(ty) = types
            .iter()
            .find(|d| d.type_id == current)
            .or(types.first())
        else {
            return actions;
        };

        ui.horizontal(|ui| {
            ui.strong(&t.name);
            ui.label("Instrument");
            let mut chosen = ty.type_id;
            egui::ComboBox::from_id_salt("instrument_type")
                .selected_text(ty.name)
                .show_ui(ui, |ui| {
                    for d in types {
                        ui.selectable_value(&mut chosen, d.type_id, d.name);
                    }
                });
            if chosen != ty.type_id {
                let id = slot.as_ref().map_or(DeviceId(project.next_id), |s| s.id);
                let mut devices = t.devices.clone();
                let new = DeviceSlot::builtin(id, chosen);
                match devices.first_mut() {
                    Some(d) => *d = new,
                    None => devices.push(new),
                }
                actions.push(UiAction::NewDevice {
                    track: t.id,
                    devices,
                });
            }
            ui.separator();
            ui.menu_button("Presets", |ui| {
                for (name, values) in &ty.presets {
                    if ui.button(name).clicked() {
                        let mut devices = t.devices.clone();
                        let id = slot.as_ref().map_or(DeviceId(project.next_id), |s| s.id);
                        let mut d = DeviceSlot::builtin(id, ty.type_id);
                        if let (
                            DeviceKind::Builtin {
                                params, samples, ..
                            },
                            Some(old),
                        ) = (&mut d.kind, slot.as_ref())
                        {
                            *params = values.clone();
                            *samples = old.samples().to_vec();
                        }
                        match devices.first_mut() {
                            Some(x) => *x = d,
                            None => devices.push(d),
                        }
                        actions.push(UiAction::NewDevice {
                            track: t.id,
                            devices,
                        });
                        ui.close();
                    }
                }
            });
            ui.add(
                egui::TextEdit::singleline(&mut self.preset_name)
                    .hint_text("preset name")
                    .desired_width(110.0),
            );
            if ui
                .add_enabled(
                    !self.preset_name.trim().is_empty(),
                    egui::Button::new("Save preset"),
                )
                .clicked()
            {
                actions.push(UiAction::SavePreset {
                    type_id: ty.type_id.to_owned(),
                    name: self.preset_name.trim().to_owned(),
                    params: slot
                        .as_ref()
                        .map(|s| s.params().to_vec())
                        .unwrap_or_default(),
                });
                self.preset_name.clear();
            }
        });
        ui.separator();

        let value =
            |id: u64, default: f32| slot.as_ref().and_then(|s| s.param(id)).unwrap_or(default);
        let device = slot.as_ref().map(|s| s.id);
        if ty.uses_samples {
            let drum = ty.drum_param.is_some_and(|p| value(p, 0.0) > 0.5);
            let samples = slot
                .as_ref()
                .map(|s| s.samples().to_vec())
                .unwrap_or_default();
            ui.horizontal_wrapped(|ui| {
                if drum {
                    for key in FIRST_PAD..FIRST_PAD + PADS {
                        let name = samples
                            .iter()
                            .find(|z| z.key == key)
                            .map_or("—".to_owned(), |z| asset_name(z.asset));
                        ui.group(|ui| {
                            ui.set_width(110.0);
                            ui.vertical(|ui| {
                                ui.small(key_name(key));
                                ui.add(egui::Label::new(name).truncate());
                                if ui.small_button("Load…").clicked() {
                                    actions.push(UiAction::LoadSample {
                                        track: t.id,
                                        key: Some(key),
                                    });
                                }
                            });
                        });
                    }
                } else {
                    let name = samples
                        .first()
                        .map_or("No sample loaded".to_owned(), |z| asset_name(z.asset));
                    ui.label(format!("Sample: {name}"));
                    if ui.button("Load sample…").clicked() {
                        actions.push(UiAction::LoadSample {
                            track: t.id,
                            key: None,
                        });
                    }
                }
            });
            ui.separator();
        }

        ui.horizontal_wrapped(|ui| {
            for p in ty.params {
                let mut v = value(p.id.0, p.default);
                let (changed, merge) = match p.kind {
                    ParamKind::Toggle => {
                        let mut on = v > 0.5;
                        let r = ui.checkbox(&mut on, p.name);
                        v = if on { 1.0 } else { 0.0 };
                        (r.changed(), false)
                    }
                    ParamKind::Choice(opts) => {
                        let mut idx = v.round() as usize;
                        let before = idx;
                        ui.vertical(|ui| {
                            ui.small(p.name);
                            egui::ComboBox::from_id_salt(("param", p.id.0))
                                .width(80.0)
                                .selected_text(opts.get(idx).copied().unwrap_or("?"))
                                .show_ui(ui, |ui| {
                                    for (i, o) in opts.iter().enumerate() {
                                        ui.selectable_value(&mut idx, i, *o);
                                    }
                                });
                        });
                        v = idx as f32;
                        (idx != before, false)
                    }
                    _ => {
                        let (r, c) = knob(ui, p, &mut v);
                        (c, r.dragged() && !r.drag_started())
                    }
                };
                if changed {
                    match device {
                        Some(device) => actions.push(UiAction::Edit {
                            edit: Edit::SetDeviceParam {
                                track: t.id,
                                device,
                                param: p.id.0,
                                value: (v != p.default).then_some(v),
                            },
                            merge,
                        }),
                        None => {
                            let mut d = DeviceSlot::builtin(DeviceId(project.next_id), ty.type_id);
                            d.set_param(p.id.0, Some(v));
                            actions.push(UiAction::NewDevice {
                                track: t.id,
                                devices: vec![d],
                            });
                        }
                    }
                }
            }
        });
        if let Some(root) = ty.root_param.filter(|_| ty.uses_samples) {
            let key = value(root, 60.0).round() as u8;
            ui.weak(format!(
                "Root key: {} — the sample plays at its original pitch on this key.",
                key_name(key)
            ));
        }
        actions
    }
}

/// "C4"-style name of a MIDI key.
pub fn key_name(key: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!(
        "{}{}",
        NAMES[usize::from(key % 12)],
        i32::from(key / 12) - 1
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names() {
        assert_eq!(key_name(60), "C4");
        assert_eq!(key_name(36), "C2");
        assert_eq!(key_name(61), "C#4");
    }
}
