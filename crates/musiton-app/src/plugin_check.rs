//! Plugin self-test (`musiton --check-plugins <report file>`): finds every installed
//! plugin and checks, each in its own process, that it loads, makes sound, keeps its
//! settings, runs in the sandbox and opens its editor. Writes a plain-text report.
//!
//! Used to test plugins on a machine where nobody can listen (remote checks).

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use musiton_core::{TempoMap, TimeSig};
use musiton_engine::node::{AudioIo, AudioNode, Event, EventKind, ProcessContext};
use musiton_plugin_host::{find_plugin_files, Plugin, PluginFormat, PluginInfo};

use crate::plugins::{run_process, run_scanner, SCAN_ARG};

/// Switch for the whole check: `--check-plugins <report file>`.
pub const CHECK_ALL_ARG: &str = "--check-plugins";
/// Switch for checking one plugin: `--check-plugin <vst3|clap> <path> <id> <0|1>`.
pub const CHECK_ONE_ARG: &str = "--check-plugin";

const RATE: f64 = 48_000.0;
const BLOCK: usize = 256;
const BLOCKS: u64 = 200;
/// Time one plugin may take for all its checks.
const CHECK_TIMEOUT: Duration = Duration::from_secs(120);

/// Checks all plugins in the standard folders and writes the report to `report`.
pub fn check_all(report: &Path) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let mut text = String::new();
    let write = |text: &str| std::fs::write(report, text);
    let files = find_plugin_files(&musiton_plugin_host::default_dirs());
    let _ = writeln!(text, "Musiton plugin check: {} plugin files", files.len());
    write(&text)?;
    for file in files {
        let mut cmd = Command::new(&exe);
        cmd.arg(SCAN_ARG).arg(&file);
        let list: Vec<PluginInfo> = match run_scanner(cmd, Duration::from_secs(20)) {
            Ok(list) => list,
            Err(e) => {
                let _ = writeln!(text, "\n{}\n  FAIL scan: {e}", file.display());
                write(&text)?;
                continue;
            }
        };
        for p in list {
            let _ = writeln!(
                text,
                "\n{} ({}, {})",
                p.name,
                p.format.name(),
                if p.instrument { "instrument" } else { "effect" }
            );
            let mut cmd = Command::new(&exe);
            cmd.arg(CHECK_ONE_ARG)
                .arg(format_arg(p.format))
                .arg(&p.path)
                .arg(&p.id)
                .arg(if p.instrument { "1" } else { "0" });
            match run_process(cmd, CHECK_TIMEOUT) {
                Ok((status, out, _)) => {
                    // One JSON string per line, so lines written before a crash survive.
                    // "... step" lines only matter when the process ended during that step.
                    let mut finished = false;
                    let mut step = String::new();
                    for line in out.lines() {
                        match serde_json::from_str::<String>(line) {
                            Ok(l) if l == END => finished = true,
                            Ok(l) if l.starts_with("...") => step = l,
                            Ok(l) => {
                                let _ = writeln!(text, "  {l}");
                            }
                            Err(_) => {}
                        }
                    }
                    if !finished {
                        let how = match status {
                            Some(s) => format!("ended early ({s})"),
                            None => format!("did not finish within {} s", CHECK_TIMEOUT.as_secs()),
                        };
                        let _ = writeln!(
                            text,
                            "  FAIL the check process {how} while {}",
                            step.trim_start_matches('.').trim()
                        );
                    }
                }
                Err(e) => {
                    let _ = writeln!(text, "  FAIL check process: {e}");
                }
            }
            write(&text)?;
        }
    }
    text.push_str("\nDone.\n");
    write(&text)?;
    Ok(())
}

fn format_arg(f: PluginFormat) -> &'static str {
    match f {
        PluginFormat::Vst3 => "vst3",
        PluginFormat::Clap => "clap",
    }
}

/// Last line of a check that ran to the end.
const END: &str = "end";

/// Checks one plugin (`args` after [`CHECK_ONE_ARG`]); prints each result as soon as it is
/// known, as one JSON string per line, and [`END`] last.
pub fn check_one(args: &[String]) -> ! {
    let mut emit = |line: String| {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{}", serde_json::to_string(&line).unwrap_or_default());
        let _ = out.flush();
    };
    match args {
        [format, path, id, instrument] => {
            let format = if format == "clap" {
                PluginFormat::Clap
            } else {
                PluginFormat::Vst3
            };
            run_checks(format, Path::new(path), id, instrument == "1", &mut emit);
        }
        _ => emit("FAIL bad arguments".to_owned()),
    }
    emit(END.to_owned());
    std::process::exit(0)
}

fn ok(cond: bool) -> &'static str {
    if cond {
        "ok  "
    } else {
        "FAIL"
    }
}

fn run_checks(
    format: PluginFormat,
    path: &Path,
    id: &str,
    instrument: bool,
    out: &mut dyn FnMut(String),
) {
    out("... loading".to_owned());
    let plugin = match Plugin::load(format, path, id, RATE, BLOCK) {
        Ok(p) => p,
        Err(e) => return out(format!("FAIL load: {e}")),
    };
    out(format!(
        "ok   load: {} parameters, latency {} samples, editor: {}",
        plugin.params().len(),
        plugin.latency(),
        if plugin.has_editor() { "yes" } else { "no" }
    ));
    out("... playing".to_owned());
    let peak = render(&mut *plugin.node(), instrument, false);
    out(format!("{} sound: peak {peak:.3}", ok(peak > 0.001)));

    out("... saving and restoring settings".to_owned());
    match plugin.state() {
        Ok(state) => match plugin.set_state(&state) {
            Ok(()) => {
                let again = render(&mut *plugin.node(), instrument, false);
                out(format!(
                    "{} settings saved and restored ({} bytes); peak afterwards {again:.3}",
                    ok(again > 0.001),
                    state.len()
                ));
            }
            Err(e) => out(format!("FAIL restore settings: {e}")),
        },
        Err(e) => out(format!("FAIL save settings: {e}")),
    }
    let mut plugin = plugin;
    if plugin.has_editor() {
        out("... opening the editor".to_owned());
        match plugin.open_editor("Musiton plugin check") {
            Ok(()) => {
                for _ in 0..30 {
                    plugin.poll_editor();
                    std::thread::sleep(Duration::from_millis(20));
                }
                let open = plugin.editor_open();
                plugin.close_editor();
                out(format!("{} editor opened and closed", ok(open)));
            }
            Err(e) => out(format!("FAIL editor: {e}")),
        }
    }
    out("... unloading".to_owned());
    drop(plugin);
    out("... loading in the sandbox".to_owned());

    match std::env::current_exe()
        .map_err(|e| e.to_string())
        .and_then(|exe| {
            Plugin::load_sandboxed(&exe, format, path, id, RATE, BLOCK).map_err(|e| e.to_string())
        }) {
        Ok(plugin) => {
            let peak = render(&mut *plugin.node(), instrument, true);
            let failure = plugin.failure();
            out(format!(
                "{} sandbox: peak {peak:.3}{}",
                ok(peak > 0.001 && failure.is_none()),
                failure.map(|f| format!(" ({f})")).unwrap_or_default()
            ));
        }
        Err(e) => out(format!("FAIL sandbox load: {e}")),
    }
}

/// Plays a note (instrument) or a tone (effect) through `node`; returns the output peak.
fn render(node: &mut dyn AudioNode, instrument: bool, slow: bool) -> f32 {
    let tempo = TempoMap::constant(120.0);
    let mut ins = vec![vec![0.0; BLOCK]; node.num_inputs()];
    let mut outs = vec![vec![0.0; BLOCK]; node.num_outputs()];
    let mut peak = 0.0_f32;
    for b in 0..BLOCKS {
        let pos = b * BLOCK as u64;
        if !instrument {
            for ch in &mut ins {
                for (i, s) in ch.iter_mut().enumerate() {
                    let t = (pos + i as u64) as f32 / RATE as f32;
                    *s = if b < BLOCKS / 2 {
                        0.5 * (std::f32::consts::TAU * 440.0 * t).sin()
                    } else {
                        0.0
                    };
                }
            }
        }
        // The note starts a little later: some plugins finish loading settings in the
        // first blocks after a restore and drop notes meanwhile.
        let note = match b {
            20 => Some(EventKind::NoteOn {
                ch: 0,
                key: 60,
                vel: 0.8,
            }),
            120 => Some(EventKind::NoteOff { ch: 0, key: 60 }),
            _ => None,
        };
        let events: Vec<Event> = note
            .filter(|_| instrument)
            .map(|kind| Event { offset: 0, kind })
            .into_iter()
            .collect();
        let ctx = ProcessContext {
            sample_rate: RATE as f32,
            frames: BLOCK,
            playing: true,
            sample_pos: pos,
            tempo: &tempo,
            time_sig: TimeSig::default(),
            metronome: false,
            inputs: &[],
            recording: false,
            punch: None,
            live_midi: &[],
        };
        node.process(&ctx, &mut AudioIo::new(&ins, &mut outs, BLOCK), &events);
        peak = outs
            .iter()
            .flatten()
            .fold(peak, |m, s| if s.is_finite() { m.max(s.abs()) } else { m });
        if slow {
            // Give the sandbox process time, like a real audio callback would.
            std::thread::sleep(Duration::from_millis(3));
        }
    }
    peak
}
