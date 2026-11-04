//! Device panel: the selected track's instrument (type, presets, samples, knobs) and its
//! effect rack (add, remove, reorder, bypass, drag to another track, EQ display).

use std::collections::HashMap;

use egui::{Color32, Pos2, Sense, Stroke, Ui};
use musiton_core::ids::{AssetId, DeviceId, TrackId};
use musiton_core::params::{ParamInfo, ParamKind};
use musiton_project::{DeviceKind, DeviceSlot, Edit, Project, Track, TrackKind};

use crate::UiAction;

/// Lowest pad key in drum mode (C2).
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

/// Drag payload: an effect being dragged to another track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FxDrag {
    /// Track it comes from.
    pub track: TrackId,
    /// The effect.
    pub device: DeviceId,
}

/// What an EQ displays: its response curve and the current spectrum (dB at `freqs`).
#[derive(Debug, Clone, Default)]
pub struct EqView {
    /// Frequencies (Hz, log-spaced).
    pub freqs: Vec<f32>,
    /// Response in dB.
    pub curve: Vec<f32>,
    /// Spectrum in dB.
    pub spectrum: Vec<f32>,
}

/// Inputs for [`DevicePanel::ui`].
pub struct PanelInput<'a> {
    /// The selected track.
    pub track: Option<TrackId>,
    /// Instrument types.
    pub instruments: &'a [DeviceType],
    /// Effect types.
    pub effects: &'a [DeviceType],
    /// Names of audio files.
    pub asset_name: &'a dyn Fn(AssetId) -> String,
    /// EQ displays per device.
    pub eq_views: &'a HashMap<DeviceId, EqView>,
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
    /// Draws the device chain of the selected track.
    pub fn ui(&mut self, ui: &mut Ui, project: &Project, input: &PanelInput) -> Vec<UiAction> {
        let mut actions = Vec::new();
        let Some(t) = input
            .track
            .and_then(|id| project.track(id))
            .filter(|t| t.kind != TrackKind::Folder)
        else {
            ui.weak("Select a track to see its instrument and effects.");
            return actions;
        };
        if t.kind == TrackKind::Instrument {
            self.instrument(ui, project, t, input, &mut actions);
            ui.separator();
        } else {
            ui.strong(&t.name);
        }
        effects(ui, project, t, input, &mut actions);
        actions
    }

    fn instrument(
        &mut self,
        ui: &mut Ui,
        project: &Project,
        t: &Track,
        input: &PanelInput,
        actions: &mut Vec<UiAction>,
    ) {
        let types = input.instruments;
        let slot = t.devices.first().cloned();
        let current = slot
            .as_ref()
            .map_or(types.first().map_or("", |d| d.type_id), |s| s.type_id());
        let Some(ty) = types
            .iter()
            .find(|d| d.type_id == current)
            .or(types.first())
        else {
            return;
        };
        let replace_instrument = |new: DeviceSlot| {
            let mut devices = t.devices.clone();
            match devices.first_mut() {
                Some(d) => *d = new,
                None => devices.push(new),
            }
            UiAction::NewDevice {
                track: t.id,
                devices,
            }
        };
        let slot_id = slot.as_ref().map_or(DeviceId(project.next_id), |s| s.id);
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
                actions.push(replace_instrument(DeviceSlot::builtin(slot_id, chosen)));
            }
            ui.separator();
            ui.menu_button("Presets", |ui| {
                for (name, values) in &ty.presets {
                    if ui.button(name).clicked() {
                        let mut d = DeviceSlot::builtin(slot_id, ty.type_id);
                        let DeviceKind::Builtin {
                            params, samples, ..
                        } = &mut d.kind;
                        *params = values.clone();
                        *samples = slot
                            .as_ref()
                            .map(|s| s.samples().to_vec())
                            .unwrap_or_default();
                        actions.push(replace_instrument(d));
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

        let value =
            |id: u64, default: f32| slot.as_ref().and_then(|s| s.param(id)).unwrap_or(default);
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
                            .map_or("—".to_owned(), |z| (input.asset_name)(z.asset));
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
                    let name = samples.first().map_or("No sample loaded".to_owned(), |z| {
                        (input.asset_name)(z.asset)
                    });
                    ui.label(format!("Sample: {name}"));
                    if ui.button("Load sample…").clicked() {
                        actions.push(UiAction::LoadSample {
                            track: t.id,
                            key: None,
                        });
                    }
                }
            });
        }
        match &slot {
            Some(d) => param_knobs(ui, t.id, d, ty, actions),
            None => {
                // No device yet: the first change creates it.
                let d = DeviceSlot::builtin(slot_id, ty.type_id);
                let mut pending = Vec::new();
                param_knobs(ui, t.id, &d, ty, &mut pending);
                for a in pending {
                    if let UiAction::Edit {
                        edit: Edit::SetDeviceParam { param, value, .. },
                        ..
                    } = a
                    {
                        let mut d = d.clone();
                        d.set_param(param, value);
                        actions.push(replace_instrument(d));
                    }
                }
            }
        }
        if let Some(root) = ty.root_param.filter(|_| ty.uses_samples) {
            let key = value(root, 60.0).round() as u8;
            ui.weak(format!(
                "Root key: {} — the sample plays at its original pitch on this key.",
                key_name(key)
            ));
        }
    }
}

/// The effect rack of track `t`.
fn effects(
    ui: &mut Ui,
    project: &Project,
    t: &Track,
    input: &PanelInput,
    actions: &mut Vec<UiAction>,
) {
    let instrument = t.kind == TrackKind::Instrument;
    let first = usize::from(instrument && !t.devices.is_empty());
    let chain: Vec<DeviceSlot> = t.devices.get(first..).unwrap_or(&[]).to_vec();
    let set_chain = |chain: Vec<DeviceSlot>| {
        let mut devices: Vec<DeviceSlot> = t.devices.get(..first).unwrap_or(&[]).to_vec();
        // An instrument track's first device is its instrument: create the default one.
        if instrument && devices.is_empty() {
            if let Some(ty) = input.instruments.first() {
                devices.push(DeviceSlot::builtin(DeviceId(project.next_id), ty.type_id));
            }
        }
        devices.extend(chain);
        UiAction::NewDevice {
            track: t.id,
            devices,
        }
    };
    ui.horizontal(|ui| {
        ui.strong("Effects");
        ui.menu_button("+ Add effect", |ui| {
            for ty in input.effects {
                if ui.button(ty.name).clicked() {
                    let mut c = chain.clone();
                    c.push(DeviceSlot::builtin(DeviceId(project.next_id), ty.type_id));
                    actions.push(set_chain(c));
                    ui.close();
                }
            }
        });
        if chain.is_empty() {
            ui.weak("No effects yet.");
        } else {
            ui.weak("Drag an effect's name onto another track's header to move it there.");
        }
    });
    for (i, d) in chain.iter().enumerate() {
        let Some(ty) = input.effects.iter().find(|x| x.type_id == d.type_id()) else {
            continue;
        };
        ui.group(|ui| {
            ui.horizontal(|ui| {
                let id = egui::Id::unique(("fx-drag", d.id));
                ui.dnd_drag_source(
                    id,
                    FxDrag {
                        track: t.id,
                        device: d.id,
                    },
                    |ui| {
                        ui.strong(ty.name);
                    },
                );
                let mut on = !d.bypass;
                if ui
                    .checkbox(&mut on, "On")
                    .on_hover_text("Off = bypassed")
                    .changed()
                {
                    let mut c = chain.clone();
                    if let Some(x) = c.get_mut(i) {
                        x.bypass = !on;
                    }
                    actions.push(set_chain(c));
                }
                if ui
                    .add_enabled(i > 0, egui::Button::new("<").small())
                    .on_hover_text("Move earlier")
                    .clicked()
                {
                    let mut c = chain.clone();
                    c.swap(i, i - 1);
                    actions.push(set_chain(c));
                }
                if ui
                    .add_enabled(i + 1 < chain.len(), egui::Button::new(">").small())
                    .on_hover_text("Move later")
                    .clicked()
                {
                    let mut c = chain.clone();
                    c.swap(i, i + 1);
                    actions.push(set_chain(c));
                }
                if ui.small_button("x").on_hover_text("Remove").clicked() {
                    let mut c = chain.clone();
                    c.remove(i);
                    actions.push(set_chain(c));
                }
            });
            if let Some(view) = input.eq_views.get(&d.id) {
                eq_display(ui, view);
            }
            param_knobs(ui, t.id, d, ty, actions);
        });
    }
}

/// Knobs (and switches/choices) for every parameter of device `d`.
fn param_knobs(
    ui: &mut Ui,
    track: TrackId,
    d: &DeviceSlot,
    ty: &DeviceType,
    actions: &mut Vec<UiAction>,
) {
    ui.horizontal_wrapped(|ui| {
        for p in ty.params {
            let mut v = d.param(p.id.0).unwrap_or(p.default);
            let (changed, merge) = param_widget(ui, d.id, p, &mut v);
            if changed {
                actions.push(UiAction::Edit {
                    edit: Edit::SetDeviceParam {
                        track,
                        device: d.id,
                        param: p.id.0,
                        value: (v != p.default).then_some(v),
                    },
                    merge,
                });
            }
        }
    });
}

/// One parameter control; returns (changed, part of an ongoing drag).
fn param_widget(ui: &mut Ui, device: DeviceId, p: &ParamInfo, v: &mut f32) -> (bool, bool) {
    match p.kind {
        ParamKind::Toggle => {
            let mut on = *v > 0.5;
            let r = ui.checkbox(&mut on, p.name);
            *v = if on { 1.0 } else { 0.0 };
            (r.changed(), false)
        }
        ParamKind::Choice(opts) => {
            let mut idx = v.round() as usize;
            let before = idx;
            ui.vertical(|ui| {
                ui.small(p.name);
                egui::ComboBox::from_id_salt(("param", device, p.id))
                    .width(80.0)
                    .selected_text(opts.get(idx).copied().unwrap_or("?"))
                    .show_ui(ui, |ui| {
                        for (i, o) in opts.iter().enumerate() {
                            ui.selectable_value(&mut idx, i, *o);
                        }
                    });
            });
            *v = idx as f32;
            (idx != before, false)
        }
        _ => {
            let (r, c) = ui.push_id((device, p.id), |ui| knob(ui, p, v)).inner;
            (c, r.dragged() && !r.drag_started())
        }
    }
}

/// EQ response curve over the live spectrum.
fn eq_display(ui: &mut Ui, view: &EqView) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().min(520.0), 130.0),
        Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, Color32::from_gray(16));
    let n = view.freqs.len().max(2);
    let x = |i: usize| rect.left() + rect.width() * i as f32 / (n - 1) as f32;
    // ±24 dB for the curve, −90…0 dB for the spectrum.
    let y_curve = |db: f32| rect.center().y - db.clamp(-24.0, 24.0) / 24.0 * rect.height() * 0.45;
    let y_spec = |db: f32| rect.bottom() - ((db + 90.0) / 90.0).clamp(0.0, 1.0) * rect.height();
    for f in [100.0_f32, 1_000.0, 10_000.0] {
        let t = (f / 20.0).log10() / 3.0;
        painter.vline(
            rect.left() + rect.width() * t,
            rect.y_range(),
            Stroke::new(1.0, Color32::from_gray(40)),
        );
    }
    painter.hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(1.0, Color32::from_gray(50)),
    );
    let spec: Vec<Pos2> = view
        .spectrum
        .iter()
        .enumerate()
        .map(|(i, d)| Pos2::new(x(i), y_spec(*d)))
        .collect();
    if spec.len() > 1 {
        painter.add(egui::Shape::line(
            spec,
            Stroke::new(1.0, Color32::from_rgb(90, 120, 160)),
        ));
    }
    let curve: Vec<Pos2> = view
        .curve
        .iter()
        .enumerate()
        .map(|(i, d)| Pos2::new(x(i), y_curve(*d)))
        .collect();
    if curve.len() > 1 {
        painter.add(egui::Shape::line(
            curve,
            Stroke::new(2.0, Color32::from_rgb(255, 210, 80)),
        ));
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
