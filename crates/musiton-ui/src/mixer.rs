//! Mixer: one channel strip per track plus the master.

use std::collections::HashMap;

use egui::{Color32, Pos2, Rect, Sense, Stroke, Ui};
use musiton_core::ids::TrackId;
use musiton_core::units::gain_to_db;
use musiton_project::{
    ChannelStrip, Edit, InputRoute, MonitorMode, OutputRoute, Project, SendRoute, Track, TrackKind,
};

use crate::UiAction;

/// Lowest fader position in dB (treated as silence).
pub const FADER_MIN_DB: f32 = -60.0;
/// Highest fader position in dB.
pub const FADER_MAX_DB: f32 = 6.0;
/// Height reserved for output routing and sends.
const ROUTING_HEIGHT: f32 = 136.0;
/// Width of one strip.
const STRIP_WIDTH: f32 = 96.0;
/// Height of fader and meter.
const FADER_HEIGHT: f32 = 160.0;
/// Peak display falls this many dB per second.
const PEAK_FALL_DB_PER_S: f32 = 20.0;

/// Levels measured since the last frame: (peak, RMS) per channel, linear.
pub type Levels = HashMap<Option<TrackId>, ([f32; 2], [f32; 2])>;

/// Displayed meter state of one strip.
#[derive(Debug, Clone, Copy, Default)]
struct MeterState {
    peak_db: [f32; 2],
    rms_db: [f32; 2],
    clipped: bool,
}

/// Mixer view state (meter ballistics).
#[derive(Debug, Default)]
pub struct Mixer {
    meters: HashMap<Option<TrackId>, MeterState>,
    /// Channels of the audio input device (0 = no input).
    pub input_channels: usize,
}

impl Mixer {
    /// Draws the mixer. `levels` are this frame's meter readings.
    pub fn ui(&mut self, ui: &mut Ui, project: &Project, levels: &Levels) -> Vec<UiAction> {
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        for (key, (peak, rms)) in levels {
            let m = self.meters.entry(*key).or_default();
            for ch in 0..2 {
                let p = gain_to_db(peak[ch]).max(FADER_MIN_DB);
                m.peak_db[ch] = p.max(m.peak_db[ch] - PEAK_FALL_DB_PER_S * dt);
                m.rms_db[ch] = gain_to_db(rms[ch]).max(FADER_MIN_DB);
                m.clipped |= peak[ch] > 1.0;
            }
        }
        let mut actions = Vec::new();
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    for t in project
                        .tracks
                        .iter()
                        .filter(|t| t.kind != TrackKind::Folder)
                    {
                        self.strip(ui, project, Some(t), &mut actions);
                        ui.separator();
                    }
                    self.strip(ui, project, None, &mut actions);
                });
            });
        actions
    }

    fn strip(
        &mut self,
        ui: &mut Ui,
        project: &Project,
        track: Option<&Track>,
        actions: &mut Vec<UiAction>,
    ) {
        let id = track.map(|t| t.id);
        let strip = track.map_or(&project.master, |t| &t.strip);
        let mut edited = strip.clone();
        edited.gain_db = edited.gain_db.max(FADER_MIN_DB);
        let mut merge = false;
        let mut changed = false;

        ui.vertical(|ui| {
            ui.set_width(STRIP_WIDTH);
            let (name, color) = track.map_or(("Master".to_owned(), [200, 200, 200]), |t| {
                (t.name.clone(), t.color)
            });
            let (r, _) = ui.allocate_exact_size(egui::vec2(STRIP_WIDTH, 4.0), Sense::hover());
            ui.painter()
                .rect_filled(r, 1.0, Color32::from_rgb(color[0], color[1], color[2]));
            ui.add(egui::Label::new(egui::RichText::new(name).strong()).truncate());

            // Fixed-height routing area so all faders line up.
            let (area, _) =
                ui.allocate_exact_size(egui::vec2(STRIP_WIDTH, ROUTING_HEIGHT), Sense::hover());
            // A child that does not allocate in the strip again (the area is already taken).
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
            {
                let ui = &mut child;
                if let Some(t) = track {
                    self.output_combo(ui, project, t, actions);
                    if t.kind == TrackKind::Audio {
                        self.record_setup(ui, t, actions);
                    }
                    egui::ScrollArea::vertical()
                        .id_salt(("sends", t.id))
                        .max_height(ROUTING_HEIGHT - 24.0)
                        .show(ui, |ui| {
                            self.sends(ui, project, t, &mut edited, &mut changed, &mut merge);
                        });
                }
            }

            // Pan.
            let r = ui.add(egui::Slider::new(&mut edited.pan, -1.0..=1.0).show_value(false));
            if r.double_clicked() {
                edited.pan = 0.0;
            }
            if r.changed() || r.double_clicked() {
                changed = true;
                merge = r.dragged() && !r.drag_started();
            }
            ui.small(pan_text(edited.pan));

            // Fader + meter.
            ui.horizontal(|ui| {
                ui.spacing_mut().slider_width = FADER_HEIGHT;
                let r = ui.add(
                    egui::Slider::new(&mut edited.gain_db, FADER_MIN_DB..=FADER_MAX_DB)
                        .vertical()
                        .show_value(false),
                );
                if r.double_clicked() {
                    edited.gain_db = 0.0;
                }
                if r.changed() || r.double_clicked() {
                    changed = true;
                    merge = r.dragged() && !r.drag_started();
                }
                let m = self.meters.entry(id).or_default();
                if meter(ui, m) {
                    m.clipped = false;
                }
            });
            let db = if edited.gain_db <= FADER_MIN_DB {
                "−∞ dB".to_owned()
            } else {
                format!("{:+.1} dB", edited.gain_db)
            };
            ui.monospace(db);

            ui.horizontal(|ui| {
                if ui
                    .selectable_label(edited.mute, "M")
                    .on_hover_text("Mute")
                    .clicked()
                {
                    edited.mute = !edited.mute;
                    changed = true;
                }
                if track.is_some()
                    && ui
                        .selectable_label(edited.solo, "S")
                        .on_hover_text("Solo")
                        .clicked()
                {
                    edited.solo = !edited.solo;
                    changed = true;
                }
                if track.is_some_and(|t| t.kind == TrackKind::Bus)
                    && ui
                        .selectable_label(edited.solo_safe, "safe")
                        .on_hover_text("Solo safe: keeps playing when other tracks are soloed")
                        .clicked()
                {
                    edited.solo_safe = !edited.solo_safe;
                    changed = true;
                }
            });
        });
        if changed {
            if edited.gain_db <= FADER_MIN_DB {
                // Bottom of the fader = silence (db_to_gain gives 0 below −144 dB).
                edited.gain_db = -200.0;
            }
            actions.push(UiAction::Edit {
                edit: Edit::SetStrip {
                    track: id,
                    strip: edited,
                },
                merge,
            });
        }
    }

    fn record_setup(&self, ui: &mut Ui, t: &Track, actions: &mut Vec<UiAction>) {
        let mut rec = t.record;
        let label = |i: InputRoute| match i {
            InputRoute::Mono(c) => format!("In {}", c + 1),
            InputRoute::Stereo(c) => format!("In {}+{}", c + 1, c + 2),
        };
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt(("input", t.id))
                .width(52.0)
                .selected_text(label(rec.input))
                .show_ui(ui, |ui| {
                    if self.input_channels == 0 {
                        ui.weak("No input device");
                    }
                    for c in 0..self.input_channels as u16 {
                        ui.selectable_value(
                            &mut rec.input,
                            InputRoute::Mono(c),
                            label(InputRoute::Mono(c)),
                        );
                    }
                    for c in (0..self.input_channels.saturating_sub(1) as u16).step_by(2) {
                        let r = InputRoute::Stereo(c);
                        ui.selectable_value(&mut rec.input, r, label(r));
                    }
                });
            let mon = match rec.monitor {
                MonitorMode::Off => "Mon off",
                MonitorMode::Auto => "Mon auto",
                MonitorMode::On => "Mon on",
            };
            if ui
                .small_button(mon)
                .on_hover_text("Input monitoring: off / auto (while armed) / on")
                .clicked()
            {
                rec.monitor = match rec.monitor {
                    MonitorMode::Off => MonitorMode::Auto,
                    MonitorMode::Auto => MonitorMode::On,
                    MonitorMode::On => MonitorMode::Off,
                };
            }
        });
        if rec != t.record {
            actions.push(UiAction::Edit {
                edit: Edit::SetRecord {
                    id: t.id,
                    record: rec,
                },
                merge: false,
            });
        }
    }

    fn output_combo(&self, ui: &mut Ui, project: &Project, t: &Track, actions: &mut Vec<UiAction>) {
        let name_of = |o: OutputRoute| match o {
            OutputRoute::Master => "Master".to_owned(),
            OutputRoute::Bus(b) => project.track(b).map_or("?".to_owned(), |t| t.name.clone()),
        };
        let mut out = t.output;
        egui::ComboBox::from_id_salt(("output", t.id))
            .width(STRIP_WIDTH - 8.0)
            .truncate()
            .selected_text(name_of(out))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut out, OutputRoute::Master, name_of(OutputRoute::Master));
                for b in project.valid_bus_targets(t.id) {
                    ui.selectable_value(
                        &mut out,
                        OutputRoute::Bus(b),
                        name_of(OutputRoute::Bus(b)),
                    );
                }
            });
        if out != t.output {
            actions.push(UiAction::Edit {
                edit: Edit::SetOutput {
                    id: t.id,
                    output: out,
                },
                merge: false,
            });
        }
    }

    fn sends(
        &self,
        ui: &mut Ui,
        project: &Project,
        t: &Track,
        edited: &mut ChannelStrip,
        changed: &mut bool,
        merge: &mut bool,
    ) {
        let mut remove = None;
        for (i, s) in edited.sends.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                let name = project.track(s.target).map_or("?", |t| t.name.as_str());
                ui.add(egui::Label::new(egui::RichText::new(name).small()).truncate());
                if ui.small_button("×").on_hover_text("Remove send").clicked() {
                    remove = Some(i);
                }
            });
            ui.horizontal(|ui| {
                let r = ui.add(
                    egui::DragValue::new(&mut s.level_db)
                        .range(-60.0..=6.0)
                        .speed(0.2)
                        .suffix(" dB"),
                );
                if r.changed() {
                    *changed = true;
                    *merge = r.dragged() && !r.drag_started();
                }
                if ui
                    .selectable_label(s.pre_fader, "pre")
                    .on_hover_text("Pre-fader send")
                    .clicked()
                {
                    s.pre_fader = !s.pre_fader;
                    *changed = true;
                }
            });
        }
        if let Some(i) = remove {
            edited.sends.remove(i);
            *changed = true;
        }
        let targets: Vec<TrackId> = project
            .valid_bus_targets(t.id)
            .into_iter()
            .filter(|b| !edited.sends.iter().any(|s| s.target == *b))
            .collect();
        if !targets.is_empty() {
            ui.menu_button("+ Send", |ui| {
                for b in targets {
                    let name = project.track(b).map_or("?", |t| t.name.as_str());
                    if ui.button(name).clicked() {
                        edited.sends.push(SendRoute {
                            target: b,
                            ..SendRoute::default()
                        });
                        *changed = true;
                        ui.close();
                    }
                }
            });
        }
    }
}

/// Draws a stereo meter; returns `true` when the clip indicator was clicked.
fn meter(ui: &mut Ui, m: &MeterState) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(16.0, FADER_HEIGHT), Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 1.0, Color32::from_gray(20));
    let y = |db: f32| {
        let t = (db - FADER_MIN_DB) / (FADER_MAX_DB - FADER_MIN_DB);
        rect.bottom() - t.clamp(0.0, 1.0) * (rect.height() - 6.0)
    };
    for ch in 0..2 {
        let x0 = rect.left() + 1.0 + ch as f32 * 7.5;
        let col = Rect::from_x_y_ranges(x0..=x0 + 6.5, y(m.rms_db[ch])..=rect.bottom());
        let color = if m.rms_db[ch] > -6.0 {
            Color32::from_rgb(230, 200, 60)
        } else {
            Color32::from_rgb(70, 200, 110)
        };
        painter.rect_filled(col, 0.0, color);
        let py = y(m.peak_db[ch]);
        painter.hline(x0..=x0 + 6.5, py, Stroke::new(1.5, Color32::WHITE));
    }
    // 0 dB mark and clip LED.
    painter.hline(
        rect.x_range(),
        y(0.0),
        Stroke::new(1.0, Color32::from_gray(120)),
    );
    let led = Rect::from_min_max(rect.min, Pos2::new(rect.right(), rect.top() + 5.0));
    painter.rect_filled(
        led,
        0.0,
        if m.clipped {
            Color32::RED
        } else {
            Color32::from_gray(50)
        },
    );
    let clicked = resp.clicked();
    resp.on_hover_text("Peak/RMS level. Red top = clipped (click to reset)");
    clicked
}

fn pan_text(pan: f32) -> String {
    let p = (pan * 100.0).round() as i32;
    match p {
        0 => "C".into(),
        p if p < 0 => format!("L{}", -p),
        p => format!("R{p}"),
    }
}
