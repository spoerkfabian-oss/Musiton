//! Command search: Ctrl+K opens a search field over every menu command, keyboard
//! shortcut and setting. Arrow keys pick an entry, Enter runs it, Escape closes.

use eframe::egui;
use egui::{Key, Modifiers};

use crate::settings::Theme;

/// What an entry does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// A command the app runs itself.
    App(AppCommand),
    /// Presses a keyboard shortcut (for commands that live in the editors).
    Key(Key, Modifiers),
}

/// Commands the app runs directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum AppCommand {
    New,
    Open,
    Save,
    SaveAs,
    ExportAudio,
    ImportMidi,
    ExportMidi,
    ImportVideo,
    ShowVideo,
    Undo,
    Redo,
    CaptureAutomation,
    Settings,
    AudioSettings,
    Plugins,
    Theme(Theme),
    Demo,
    Shortcuts,
    Quit,
}

/// One searchable entry: name, extra search words, shown keys, action.
struct Entry {
    name: &'static str,
    words: &'static str,
    keys: &'static str,
    action: Action,
}

const fn app(name: &'static str, words: &'static str, keys: &'static str, c: AppCommand) -> Entry {
    Entry {
        name,
        words,
        keys,
        action: Action::App(c),
    }
}

const fn key(name: &'static str, words: &'static str, keys: &'static str, k: Key) -> Entry {
    Entry {
        name,
        words,
        keys,
        action: Action::Key(k, Modifiers::NONE),
    }
}

const fn ctrl(name: &'static str, words: &'static str, keys: &'static str, k: Key) -> Entry {
    Entry {
        name,
        words,
        keys,
        action: Action::Key(k, Modifiers::COMMAND),
    }
}

const fn ctrl_shift(name: &'static str, words: &'static str, keys: &'static str, k: Key) -> Entry {
    Entry {
        name,
        words,
        keys,
        action: Action::Key(
            k,
            Modifiers {
                shift: true,
                ..Modifiers::COMMAND
            },
        ),
    }
}

/// Every entry, in the order shown for an empty search.
const ENTRIES: &[Entry] = &[
    key("Play / stop", "start transport", "Space", Key::Space),
    key(
        "Return to start",
        "home rewind beginning",
        "Home",
        Key::Home,
    ),
    key("Loop on/off", "cycle repeat", "L", Key::L),
    key("Record", "recording armed", "R", Key::R),
    key("Automation view on/off", "envelope lanes", "A", Key::A),
    ctrl("Add audio track", "new track", "Ctrl+T", Key::T),
    ctrl_shift(
        "Add instrument track",
        "new track midi synth",
        "Ctrl+Shift+T",
        Key::T,
    ),
    ctrl("Duplicate selected clips", "copy", "Ctrl+D", Key::D),
    key("Split clips at the playhead", "cut", "S", Key::S),
    key(
        "Delete selected clips or track",
        "remove",
        "Delete",
        Key::Delete,
    ),
    key("Rename selected track", "name", "F2", Key::F2),
    app("Undo", "", "Ctrl+Z", AppCommand::Undo),
    app("Redo", "", "Ctrl+Y", AppCommand::Redo),
    app(
        "Capture automation",
        "knob fader moves last playback record afterwards",
        "",
        AppCommand::CaptureAutomation,
    ),
    app("New project", "song file", "Ctrl+N", AppCommand::New),
    app(
        "Open project…",
        "song file load",
        "Ctrl+O",
        AppCommand::Open,
    ),
    app("Save", "song file", "Ctrl+S", AppCommand::Save),
    app(
        "Save as…",
        "song file copy",
        "Ctrl+Shift+S",
        AppCommand::SaveAs,
    ),
    app(
        "Export audio…",
        "render bounce mixdown wav flac mp3 ogg stems",
        "",
        AppCommand::ExportAudio,
    ),
    app("Import MIDI file…", "mid", "", AppCommand::ImportMidi),
    app("Export MIDI file…", "mid", "", AppCommand::ExportMidi),
    app("Import video…", "film movie", "", AppCommand::ImportVideo),
    app("Show video window", "film movie", "", AppCommand::ShowVideo),
    app(
        "Plugins…",
        "vst3 clap scan installed",
        "",
        AppCommand::Plugins,
    ),
    app(
        "Settings…",
        "options preferences interface size zoom autosave",
        "",
        AppCommand::Settings,
    ),
    app(
        "Audio and MIDI settings…",
        "device output input sample rate buffer size latency midi keyboard",
        "",
        AppCommand::AudioSettings,
    ),
    app(
        "Theme: dark",
        "colours colors appearance",
        "",
        AppCommand::Theme(Theme::Dark),
    ),
    app(
        "Theme: light",
        "colours colors appearance",
        "",
        AppCommand::Theme(Theme::Light),
    ),
    app(
        "Theme: follow the system",
        "colours colors appearance",
        "",
        AppCommand::Theme(Theme::System),
    ),
    app("Open demo song", "example", "", AppCommand::Demo),
    app("Keyboard shortcuts", "help keys", "", AppCommand::Shortcuts),
    app("Quit", "exit close", "", AppCommand::Quit),
];

/// Whether every word of `query` appears in the entry's name or search words.
fn matches(query: &str, e: &Entry) -> bool {
    let hay = format!("{} {}", e.name, e.words).to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|w| hay.contains(w))
}

/// The command search window.
#[derive(Default)]
pub struct CommandSearch {
    open: bool,
    query: String,
    selected: usize,
}

impl CommandSearch {
    /// Opens the search with an empty field.
    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
    }

    /// Opens on Ctrl+K, draws the window and returns the action chosen this frame.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<Action> {
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::K)) {
            self.open();
        }
        if !self.open {
            return None;
        }
        let hits: Vec<&Entry> = ENTRIES.iter().filter(|e| matches(&self.query, e)).collect();
        let (up, down, enter, escape) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if down {
            self.selected = (self.selected + 1).min(hits.len().saturating_sub(1));
        }
        if up {
            self.selected = self.selected.saturating_sub(1);
        }
        let mut chosen = enter
            .then(|| hits.get(self.selected).map(|e| e.action))
            .flatten();
        egui::Window::new("Command search")
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 60.0])
            .show(ctx, |ui| {
                ui.set_width(420.0);
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Search commands and settings…")
                        .desired_width(f32::INFINITY),
                );
                field.request_focus();
                if field.changed() {
                    self.selected = 0;
                }
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        if hits.is_empty() {
                            ui.weak("Nothing found");
                        }
                        for (i, e) in hits.iter().enumerate() {
                            let row = ui.horizontal(|ui| {
                                let r = ui.selectable_label(i == self.selected, e.name);
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        // Room for the scroll bar.
                                        ui.add_space(14.0);
                                        ui.weak(e.keys)
                                    },
                                );
                                r
                            });
                            if i == self.selected && (up || down) {
                                row.inner.scroll_to_me(None);
                            }
                            if row.inner.clicked() {
                                chosen = Some(e.action);
                            }
                        }
                    });
            });
        if chosen.is_some() || escape {
            self.open = false;
            ctx.memory_mut(egui::Memory::stop_text_input);
        }
        chosen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(query: &str) -> Vec<&'static str> {
        ENTRIES
            .iter()
            .filter(|e| matches(query, e))
            .map(|e| e.name)
            .collect()
    }

    #[test]
    fn search_finds_commands_by_name_and_by_other_words() {
        assert_eq!(find("instr track"), ["Add instrument track"]);
        assert!(find("buffer").contains(&"Audio and MIDI settings…"));
        assert!(find("MP3").contains(&"Export audio…"));
        assert_eq!(find("").len(), ENTRIES.len());
        assert!(find("nothing like this").is_empty());
    }
}
