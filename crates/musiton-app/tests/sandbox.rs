//! A plugin running in its own process (`musiton --plugin-host`).

use std::path::Path;

use musiton_core::{TempoMap, TimeSig};
use musiton_engine::node::{AudioIo, Event, EventKind, ProcessContext};
use musiton_plugin_host::{Plugin, PluginFormat};

fn ctx(tempo: &TempoMap, pos: u64) -> ProcessContext<'_> {
    ProcessContext {
        sample_rate: 48_000.0,
        frames: 256,
        playing: true,
        sample_pos: pos,
        tempo,
        time_sig: TimeSig::default(),
        metronome: false,
        inputs: &[],
        recording: false,
        punch: None,
        live_midi: &[],
    }
}

#[test]
fn a_sandboxed_synth_plays_and_keeps_its_state() {
    let path = Path::new("/usr/lib/clap/Kars.clap");
    let Some(info) = musiton_plugin_host::scan_file(path)
        .ok()
        .and_then(|v| v.into_iter().next())
    else {
        eprintln!("test plugin not installed; skipping");
        return;
    };
    let exe = Path::new(env!("CARGO_BIN_EXE_musiton"));
    let mut plugin =
        Plugin::load_sandboxed(exe, PluginFormat::Clap, path, &info.id, 48_000.0, 256).unwrap();
    assert!(!plugin.params().is_empty());
    // One block of extra latency for the sandbox.
    assert!(plugin.latency() >= 256);

    let tempo = TempoMap::constant(120.0);
    let mut node = plugin.node();
    let ins = vec![vec![0.0; 256]; node.num_inputs()];
    let mut outs = vec![vec![0.0; 256]; node.num_outputs()];
    let note = Event {
        offset: 0,
        kind: EventKind::NoteOn {
            ch: 0,
            key: 60,
            vel: 0.8,
        },
    };
    let mut peak = 0.0_f32;
    for b in 0..200u64 {
        let ev = if b == 0 {
            std::slice::from_ref(&note)
        } else {
            &[]
        };
        node.process(
            &ctx(&tempo, b * 256),
            &mut AudioIo::new(&ins, &mut outs, 256),
            ev,
        );
        peak = outs[0].iter().fold(peak, |m, s| m.max(s.abs()));
        // Give the child time, like a real audio callback would.
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(peak > 0.01, "peak {peak}");

    // Settings go through the pipe.
    let state = plugin.state().unwrap();
    plugin.set_state(&state).unwrap();
    for _ in 0..30 {
        plugin.take_edits();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let p = &plugin.params()[0];
    assert!(!plugin.param_text(p.id, plugin.param_value(p.id)).is_empty());
    assert!(plugin.failure().is_none());
    drop(node);

    // A plugin that cannot load reports why instead of hanging.
    let err = Plugin::load_sandboxed(
        exe,
        PluginFormat::Clap,
        Path::new("/nonexistent/X.clap"),
        "x",
        48_000.0,
        256,
    )
    .err()
    .unwrap();
    assert!(!err.to_string().is_empty());
}
