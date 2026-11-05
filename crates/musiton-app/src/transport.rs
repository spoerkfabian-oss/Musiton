//! Transport bar: play/stop, position, tempo, time signature, metronome, loop.

use eframe::egui;
use musiton_core::{BarBeatTick, SamplePos, TempoMap, Tick, TimeSig};
use musiton_engine::{EngineCommand, EngineHandle};
use musiton_project::{Edit, Project};

/// Keeps the engine's transport settings in step with the project (sends only changes).
#[derive(Default)]
pub struct EngineSync {
    tempo: Option<TempoMap>,
    time_sig: Option<TimeSig>,
    loop_range: Option<Option<(Tick, Tick)>>,
    metronome: Option<bool>,
    punch: Option<Option<(Tick, Tick)>>,
}

impl EngineSync {
    /// Forgets what was sent (call after starting a new engine).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Sends whatever changed in `project` since the last call.
    pub fn sync(&mut self, project: &Project, engine: &mut EngineHandle) {
        if self.tempo.as_ref() != Some(&project.tempo) {
            self.tempo = Some(project.tempo.clone());
            engine.send_or_log(EngineCommand::SetTempoMap(Box::new(project.tempo.clone())));
        }
        if self.time_sig != Some(project.time_sig) {
            self.time_sig = Some(project.time_sig);
            engine.send_or_log(EngineCommand::SetTimeSig(project.time_sig));
        }
        let range = project.loop_range.filter(|_| project.loop_enabled);
        if self.loop_range != Some(range) {
            self.loop_range = Some(range);
            engine.send_or_log(EngineCommand::SetLoop(range));
        }
        if self.metronome != Some(project.metronome) {
            self.metronome = Some(project.metronome);
            engine.send_or_log(EngineCommand::SetMetronome(project.metronome));
        }
        let punch = project.punch_range.filter(|_| project.punch_enabled);
        if self.punch != Some(punch) {
            self.punch = Some(punch);
            engine.send_or_log(EngineCommand::SetPunch(punch));
        }
    }
}

/// Bar-based range editor ("from bar N, length M bars"). Returns the new range and
/// whether it comes from an ongoing drag.
fn bar_range(
    ui: &mut egui::Ui,
    range: Option<(Tick, Tick)>,
    bar: i64,
) -> Option<((Tick, Tick), bool)> {
    let (start, end) = range.unwrap_or((Tick(0), Tick(4 * bar)));
    let mut start_bar = start.0 / bar + 1;
    let mut len_bars = ((end.0 - start.0) / bar).max(1);
    ui.label("from bar");
    let r1 = ui.add(egui::DragValue::new(&mut start_bar).range(1..=999));
    ui.label("length");
    let r2 = ui.add(
        egui::DragValue::new(&mut len_bars)
            .range(1..=64)
            .suffix(" bars"),
    );
    if !(r1.changed() || r2.changed()) {
        return None;
    }
    let s = (start_bar - 1) * bar;
    let merge = (r1.dragged() && !r1.drag_started()) || (r2.dragged() && !r2.drag_started());
    Some(((Tick(s), Tick(s + len_bars * bar)), merge))
}

/// What the transport bar wants the app to do.
#[derive(Default)]
pub struct TransportOutput {
    /// Undoable project edits, with "merge with previous" flag (true while dragging).
    pub edits: Vec<(Edit, bool)>,
    /// Non-undoable project setting changed (loop on/off, metronome).
    pub settings_changed: bool,
}

/// Transport bar UI state.
#[derive(Default)]
pub struct Transport {
    test_tone: bool,
    /// MIDI recording adds to existing notes (true) or replaces them (false).
    pub overdub: bool,
}

impl Transport {
    /// Draws the bar. Play/stop/locate go straight to the engine; project changes are
    /// returned as edits.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        project: &mut Project,
        engine: Option<&mut EngineHandle>,
    ) -> TransportOutput {
        let mut out = TransportOutput::default();
        let playing = engine.as_ref().is_some_and(|e| e.shared().playing());
        let recording = engine.as_ref().is_some_and(|e| e.shared().recording());
        let mut cmds = Vec::new();

        // Consumed here so a focused button doesn't also react to the same key press.
        // Ignored while a text field has keyboard focus.
        let typing = musiton_ui::text_input_focused(ui.ctx());
        let none = egui::Modifiers::NONE;
        let key = |k| !typing && ui.input_mut(|i| i.consume_key(none, k));
        let space = key(egui::Key::Space);
        let home = key(egui::Key::Home);
        let rec_key = key(egui::Key::R);
        if key(egui::Key::L) {
            project.loop_enabled = !project.loop_enabled;
            out.settings_changed = true;
        }
        ui.horizontal(|ui| {
            if ui
                .button("|<")
                .on_hover_text("Return to start (Home)")
                .clicked()
                || home
            {
                cmds.push(EngineCommand::Locate(SamplePos(0)));
            }
            let label = if playing { "Stop" } else { "Play" };
            if ui
                .add(egui::Button::new(label).min_size(egui::vec2(56.0, 0.0)))
                .on_hover_text("Play / stop (Space)")
                .clicked()
                || space
            {
                if playing {
                    cmds.push(EngineCommand::Stop);
                    cmds.push(EngineCommand::SetRecording(false));
                } else {
                    cmds.push(EngineCommand::Play);
                }
            }
            let rec_text = egui::RichText::new("Rec").color(if recording {
                egui::Color32::from_rgb(255, 70, 70)
            } else {
                ui.visuals().text_color()
            });
            if ui
                .add(egui::Button::new(rec_text).selected(recording))
                .on_hover_text("Record on armed tracks (R)")
                .clicked()
                || rec_key
            {
                cmds.push(EngineCommand::SetRecording(!recording));
                if !recording && !playing {
                    cmds.push(EngineCommand::Play);
                }
            }
            ui.separator();

            ui.label("BPM");
            let mut bpm = project.tempo.bpm_at(0.0);
            let r = ui.add(
                egui::DragValue::new(&mut bpm)
                    .range(20.0..=300.0)
                    .speed(0.2)
                    .fixed_decimals(1),
            );
            if r.changed() {
                let merge = r.dragged() && !r.drag_started();
                out.edits
                    .push((Edit::SetTempo(TempoMap::constant(bpm)), merge));
            }

            ui.label("Time sig");
            let mut sig = project.time_sig;
            let mut sig_changed = ui
                .add(egui::DragValue::new(&mut sig.num).range(1..=32))
                .changed();
            ui.label("/");
            egui::ComboBox::from_id_salt("den")
                .width(40.0)
                .selected_text(sig.den.to_string())
                .show_ui(ui, |ui| {
                    for d in [2u8, 4, 8, 16] {
                        sig_changed |= ui
                            .selectable_value(&mut sig.den, d, d.to_string())
                            .changed();
                    }
                });
            if sig_changed {
                out.edits.push((Edit::SetTimeSig(sig), false));
            }
            ui.separator();

            if ui
                .toggle_value(&mut project.metronome, "Metronome")
                .on_hover_text("Click track while playing and recording")
                .changed()
            {
                out.settings_changed = true;
            }
            if ui
                .toggle_value(&mut project.loop_enabled, "Loop")
                .on_hover_text("Repeat the loop range (L)")
                .changed()
            {
                out.settings_changed = true;
            }
            let bar = project.time_sig.ticks_per_bar();
            if let Some((r, merge)) = bar_range(ui, project.loop_range, bar) {
                out.edits.push((Edit::SetLoop(Some(r)), merge));
            }
            ui.separator();
            if ui
                .toggle_value(&mut project.punch_enabled, "Punch")
                .on_hover_text("Record only inside the punch range")
                .changed()
            {
                out.settings_changed = true;
            }
            if project.punch_enabled {
                if let Some((r, merge)) = bar_range(ui, project.punch_range, bar) {
                    out.edits.push((Edit::SetPunch(Some(r)), merge));
                }
                if project.punch_range.is_none() {
                    out.edits.push((Edit::SetPunch(project.loop_range), false));
                }
            }
            ui.separator();

            ui.toggle_value(&mut self.overdub, "Overdub")
                .on_hover_text("MIDI recording: add to existing notes instead of replacing them");
            ui.toggle_value(&mut self.test_tone, "Test tone")
                .on_hover_text("Play a quiet 440 Hz sine to check the audio output");
        });

        if let Some(e) = engine {
            for cmd in cmds {
                e.send_or_log(cmd);
            }
            e.shared().set_test_tone(self.test_tone);

            let sr = f64::from(e.sample_rate());
            let pos = e.shared().playhead() as f64;
            let tick = project.tempo.sample_to_tick(pos, sr);
            let bbt = BarBeatTick::from_tick(Tick(tick.floor() as i64), project.time_sig);
            let secs = pos / sr;
            ui.monospace(
                egui::RichText::new(format!(
                    "{bbt}   {:02}:{:06.3}",
                    (secs / 60.0).floor(),
                    secs % 60.0
                ))
                .size(20.0),
            );
        }
        out
    }
}
