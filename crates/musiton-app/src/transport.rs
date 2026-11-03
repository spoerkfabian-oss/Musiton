//! Transport bar: play/stop, position, tempo, time signature, metronome, loop.

use eframe::egui;
use musiton_core::{BarBeatTick, SamplePos, TempoMap, Tick, TimeSig};
use musiton_engine::graph::GraphBuilder;
use musiton_engine::nodes::Metronome;
use musiton_engine::{EngineCommand, EngineHandle};

/// UI-side transport settings (will move into the project model in phase 3).
pub struct Transport {
    bpm: f64,
    time_sig: TimeSig,
    metronome: bool,
    loop_on: bool,
    loop_start_bar: u32,
    loop_bars: u32,
    test_tone: bool,
}

impl Default for Transport {
    fn default() -> Self {
        Self {
            bpm: 120.0,
            time_sig: TimeSig::default(),
            metronome: true,
            loop_on: false,
            loop_start_bar: 1,
            loop_bars: 4,
            test_tone: false,
        }
    }
}

impl Transport {
    /// Sends the whole state (and the phase-2 graph) to a freshly started engine.
    pub fn init_engine(&self, engine: &mut EngineHandle) {
        let mut b = GraphBuilder::new();
        let m = b.add(Box::new(Metronome::new()));
        b.set_output(m);
        match b.build(engine.sample_rate(), engine.max_block()) {
            Ok(g) => engine.send_or_log(EngineCommand::SwapGraph(Box::new(g))),
            Err(e) => tracing::error!("building graph failed: {e}"),
        }
        engine.send_or_log(EngineCommand::SetTempoMap(Box::new(self.tempo_map())));
        engine.send_or_log(EngineCommand::SetTimeSig(self.time_sig));
        engine.send_or_log(EngineCommand::SetMetronome(self.metronome));
        engine.send_or_log(EngineCommand::SetLoop(self.loop_range()));
        engine.shared().set_test_tone(self.test_tone);
    }

    fn tempo_map(&self) -> TempoMap {
        TempoMap::constant(self.bpm)
    }

    fn loop_range(&self) -> Option<(Tick, Tick)> {
        if !self.loop_on {
            return None;
        }
        let bar = self.time_sig.ticks_per_bar();
        let start = i64::from(self.loop_start_bar.max(1) - 1) * bar;
        Some((
            Tick(start),
            Tick(start + i64::from(self.loop_bars.max(1)) * bar),
        ))
    }

    /// Draws the bar; sends commands to `engine` when something changes.
    pub fn ui(&mut self, ui: &mut egui::Ui, mut engine: Option<&mut EngineHandle>) {
        let playing = engine.as_ref().is_some_and(|e| e.shared().playing());
        let mut cmds = Vec::new();
        let mut send = |cmd: EngineCommand| cmds.push(cmd);

        // Consumed here so a focused button doesn't also react to the same key press.
        let none = egui::Modifiers::NONE;
        let space = ui.input_mut(|i| i.consume_key(none, egui::Key::Space));
        let home = ui.input_mut(|i| i.consume_key(none, egui::Key::Home));
        ui.horizontal(|ui| {
            if ui
                .button("|<")
                .on_hover_text("Return to start (Home)")
                .clicked()
                || home
            {
                send(EngineCommand::Locate(SamplePos(0)));
            }
            let label = if playing { "Stop" } else { "Play" };
            if ui
                .add(egui::Button::new(label).min_size(egui::vec2(56.0, 0.0)))
                .on_hover_text("Play / stop (Space)")
                .clicked()
                || space
            {
                send(if playing {
                    EngineCommand::Stop
                } else {
                    EngineCommand::Play
                });
            }
            ui.separator();

            ui.label("BPM");
            if ui
                .add(
                    egui::DragValue::new(&mut self.bpm)
                        .range(20.0..=300.0)
                        .speed(0.2)
                        .fixed_decimals(1),
                )
                .changed()
            {
                send(EngineCommand::SetTempoMap(Box::new(self.tempo_map())));
                if self.loop_on {
                    send(EngineCommand::SetLoop(self.loop_range()));
                }
            }
            ui.label("Time sig");
            let mut sig_changed = ui
                .add(egui::DragValue::new(&mut self.time_sig.num).range(1..=32))
                .changed();
            ui.label("/");
            egui::ComboBox::from_id_salt("den")
                .width(40.0)
                .selected_text(self.time_sig.den.to_string())
                .show_ui(ui, |ui| {
                    for d in [2u8, 4, 8, 16] {
                        sig_changed |= ui
                            .selectable_value(&mut self.time_sig.den, d, d.to_string())
                            .changed();
                    }
                });
            if sig_changed {
                send(EngineCommand::SetTimeSig(self.time_sig));
                send(EngineCommand::SetLoop(self.loop_range()));
            }
            ui.separator();

            if ui.toggle_value(&mut self.metronome, "Metronome").changed() {
                send(EngineCommand::SetMetronome(self.metronome));
            }
            let mut loop_changed = ui.toggle_value(&mut self.loop_on, "Loop").changed();
            ui.label("from bar");
            loop_changed |= ui
                .add(egui::DragValue::new(&mut self.loop_start_bar).range(1..=999))
                .changed();
            ui.label("length");
            loop_changed |= ui
                .add(
                    egui::DragValue::new(&mut self.loop_bars)
                        .range(1..=64)
                        .suffix(" bars"),
                )
                .changed();
            if loop_changed {
                send(EngineCommand::SetLoop(self.loop_range()));
            }
            ui.separator();

            ui.toggle_value(&mut self.test_tone, "Test tone");
        });

        if let Some(e) = engine.as_deref_mut() {
            for cmd in cmds {
                e.send_or_log(cmd);
            }
            e.shared().set_test_tone(self.test_tone);
        }

        if let Some(e) = engine.as_deref() {
            let sr = f64::from(e.sample_rate());
            let pos = e.shared().playhead() as f64;
            let tick = self.tempo_map().sample_to_tick(pos, sr);
            let bbt = BarBeatTick::from_tick(Tick(tick.floor() as i64), self.time_sig);
            let secs = pos / sr;
            ui.horizontal(|ui| {
                ui.monospace(
                    egui::RichText::new(format!(
                        "{bbt}   {:02}:{:06.3}",
                        (secs / 60.0).floor(),
                        secs % 60.0
                    ))
                    .size(20.0),
                );
            });
        }
    }
}
