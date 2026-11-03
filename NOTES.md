# NOTES.md

Decisions, deviations from ARCHITECTURE.md, known issues. Newest phase at the bottom.

## Owner decisions
- Licence: MIT OR Apache-2.0.
- Name: **Musiton**. Crates are `musiton-*` (instead of `daw-*`), scanner binary `musiton-scan`, project folder extension `.musiton` (instead of `.dawproj`). Docs updated accordingly.
- Commits go straight to `main`.

## Phase 0 — Foundation
**Built:** Cargo workspace with all crates as stubs, `rust-toolchain.toml` (pinned 1.97.0), CI (Windows + Linux: fmt, clippy -D warnings, test), `tracing` logging in the app (`RUST_LOG` overrides the default `info,wgpu=warn`), empty eframe window titled "Musiton".

**Decisions**
- Library versions at start: egui/eframe 0.37 (wgpu backend), others see `Cargo.toml`. egui 0.37 changed `App::update(ctx)` to `App::ui(ui)`.
- `assert_no_alloc`: the app installs `AllocDisabler` as global allocator in **debug builds only**; audio-thread code is wrapped in `musiton_engine::rt::no_alloc(...)` (no-op in release).
- Workspace lints: `unsafe_code = "forbid"` (the plugin-host crate will opt out in phase 12), `missing_docs = "warn"` (→ error in CI via `-D warnings`).
- Dependencies of external crates are built with `opt-level = 2` in dev builds so debug audio code stays fast enough.
- Top-level `tests/` folder: integration tests live in each crate's own `tests/` folder instead; cross-crate golden tests will go into the crate that owns the offline renderer (`musiton-engine`).

**Known issues**
- None.

## Phase 1 — Audio out
**Built:** cpal output backend (`musiton-io::audio`): device list, sample-rate and buffer-size choice, "Options → Audio settings…" dialog, settings persisted to `settings.json` (Windows: `%APPDATA%\Musiton\config\settings.json`). 440 Hz test tone at −12 dBFS with 10 ms fade in/out, dropout (xrun) counter and CPU-load meter in the status bar. `musiton_engine::render_offline` renders without a device and is used by the tests.

**Decisions**
- Phase-1 control is via atomics in `EngineShared` (test tone flag, xrun count, CPU load). Command/event rings come in phase 2.
- Device is stored by cpal's stable device id (`host:id` string), shown by name. A missing device shows an error in the status bar instead of crashing.
- Sample format: F32 preferred, then I32/I16/U16 (converted in the callback from an f32 scratch buffer pre-allocated before the stream starts).
- If the driver rejects the fixed buffer size, Musiton falls back to the device default and shows the real size.
- Default buffer size in fresh settings: 256 samples.
- **RT-rule exception:** the callback calls `Instant::now()` twice per buffer for the CPU meter (QueryPerformanceCounter on Windows / vDSO on Linux — no lock, no allocation).
- Dropouts are counted when (a) the driver reports an xrun (ALSA does; WASAPI usually doesn't) or (b) one callback took longer than its buffer period.

**Known issues**
- Dropout detection on WASAPI is only the "callback too slow" heuristic; missed device deadlines caused by other system load are not seen.
- Not verified by ear in CI (no audio hardware): needs the owner's test on Windows.

## Phase 2 — Engine core
**Built:** `AudioNode` trait, `GraphBuilder` → `CompiledGraph` (topological sort, cycle detection, pre-allocated buffers, inputs summed), `rtrb` command ring UI→audio, GC thread that frees replaced graphs/tempo maps, transport (play/stop/locate/loop), `TempoMap` (step changes) with tick↔sample conversion, time signature, metronome node (accent on beat 1), click-free graph hot-swap (10 ms cross-fade), transport bar in the app.

**Decisions / deviations from ARCHITECTURE.md**
- `musiton_engine::create(sample_rate, max_block)` returns `(EngineHandle, Engine)`. The audio backend creates a fresh pair per stream (`start_output` returns the handle); the app re-sends its state + graph after every restart.
- No `EngineEvent` ring yet: playhead, playing, CPU load and xruns are atomics in `EngineShared` (the UI only needs the latest value). The event ring will be added when lossless events are needed (recording, phase 7).
- `TempoMap` is sent as `Box<TempoMap>` through the command ring instead of `arc-swap`: with `arc-swap` the audio thread could end up freeing the last reference. Old objects go to the GC thread (`Box<dyn Send>` ring); if that ring were ever full the object is leaked rather than freed on the audio thread.
- Tempo change keeps the *musical* position (the sample position is recomputed), so the metronome stays on the beat.
- Loop range is stored in ticks (follows tempo changes); the block is split exactly at the loop end. The loop only applies while the playhead is before the loop end.
- Graph hot-swap cross-fades old and new graph for 10 ms (both run during the fade). Re-using node instances across graphs (so synths/plugins keep their state) is postponed until there are stateful nodes (phase 8/9).
- `ProcessContext` carries the transport fields directly (`playing`, `sample_pos`, `tempo`, `time_sig`, `metronome`); `AudioNode` additionally declares `num_inputs`/`num_outputs` and `set_param`.
- Engine block size (`max_block`) is 1024 frames; device buffers of any size are split.
- Golden files: `crates/musiton-engine/tests/golden/*.wav` (mono f32). `MUSITON_BLESS=1 cargo test` rewrites them; tolerance 1e-5 per sample.
- Metronome: 1760 Hz accent / 880 Hz beat, 8 ms decay, 50 ms long.

**Known issues**
- The transport bar is temporary (phase 4 builds the real UI); tempo is a single constant BPM in the UI for now (the engine supports tempo maps).
- Space toggles play/stop; while a number field is being edited by keyboard, Space still toggles the transport.

## Phase 3 — Project model
**Built:** `Project` model (tracks, clips, notes, markers, tempo, time signature, loop, master strip, asset list), RON save/load with version check and migration hook, undo/redo for every edit, File menu (New, Open, Open recent, Save, Save as), Edit menu (Undo/Redo with labels), shortcuts (Ctrl+N/O/S, Ctrl+Shift+S, Ctrl+Z, Ctrl+Y / Ctrl+Shift+Z), "Save changes?" dialog on New/Open/Quit, `*` in the title for unsaved changes, autosave every 2 minutes, opening a project given on the command line (for the later file association).

**Decisions / deviations**
- **File layout (owner decision):** `MySong/MySong.musiton` (RON text) plus `audio/`, `peaks/`, `plugins/`, `autosave/` next to it. "Save as" creates the song folder unless the chosen folder already has the song's name.
- Version handling: files with a newer format are refused with a clear message; older ones go through `migrate(text, from_version)` (currently format 1, so nothing to do). Every struct uses `#[serde(default)]`, so adding fields later needs no migration; unknown fields are ignored.
- Undo uses an `Edit` enum instead of `Box<dyn EditCommand>`: "set" edits swap the stored value with the project's (apply = revert), add/remove edits carry the moved object. Consecutive changes of the same value while dragging merge into one undo step. History is capped at 1000 steps.
- The id counter (`next_id`) is deliberately *not* undone, so ids are never reused (an undone "add track" could otherwise collide with a new track).
- Fields are added to the model only when a phase uses them (devices, sends, automation, input routing come later — additive, no migration).
- Loop on/off and metronome are saved in the project but are not undo steps (they still mark the project as changed).
- Autosave writes `autosave/<name>.musiton` inside the project folder (untitled songs: `%APPDATA%\Musiton\data\autosave\`). Crash recovery from it is phase 14.
- Recent projects (max 10) are stored in `settings.json`.

**Known issues**
- The project's `sample_rate` is not yet linked to the device rate (matters from phase 5 on).
- The transport bar only edits a single constant tempo.
