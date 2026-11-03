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

## Phase 4 — UI shell
**Built:** Window layout (menu, transport bar, browser panel left, arrangement centre, Editor/Mixer tabs bottom, status bar). Arrangement view in `musiton-ui`: toolbar (+Audio/+Instrument/+Bus, snap grid, follow, zoom), bar/beat ruler (click/drag to move the playhead, loop range shown), track headers (colour, name, type, mute/solo), timeline lanes with bar/beat/snap grid and playhead. Zoom with Ctrl+wheel (around the mouse), Shift+wheel scrolls in time, wheel scrolls tracks. Snap 1/1…1/32 plus triplets and "Bar". Tracks: add, delete, rename (double-click or F2), recolour (right-click menu), reorder by dragging the header. Help → Keyboard shortcuts. `L` toggles the loop.

**Decisions**
- `musiton-ui` does not know the engine: widgets return `UiAction`s (undoable edits, locate) that the app applies.
- Only visible track rows are laid out (`ScrollArea::show_rows`); grid lines are computed once per frame and shared by all rows.
- Single-key shortcuts (Space, Home, L, Delete, F2) are ignored while a text field has keyboard focus.
- New tracks are inserted below the selected track and get the next colour of an 8-colour palette.
- Headless UI tests use `egui::Context::run_ui` directly instead of `egui_kittest` (no extra dependency needed yet).
- The Rec button is shown but disabled until phase 7.

**Performance:** 50 tracks: ~1.2 ms per frame for layout + tessellation in a debug build (test `fifty_tracks_render_well_within_a_60fps_frame` fails above 8 ms).

**Known issues**
- Browser panel and Editor/Mixer tabs are placeholders until phases 5, 6 and 8.
- Folder tracks can be created only from files for now (no UI to nest tracks yet).

## Phase 5 — Audio clips
**Built:** Import of WAV/FLAC/MP3/OGG/AIFF by dragging files from Explorer onto a track (or below the tracks for a new track) or from the new Browser panel (choose a folder; drag, or double-click to add on a new track at the playhead). Files are decoded (symphonia), converted to the project sample rate (rubato FFT resampler) and stored as 32-bit float WAV in the project's `audio/` folder, on a worker thread. Clips show their waveform from multi-resolution peaks. Editing: move (snapped, also to another track of the same kind), trim both edges, fade handles, clip gain (right-click), split at playhead (S), duplicate (Ctrl+D), delete, multi-select (Shift/Ctrl+click). Each drag is previewed and committed as a single undo step. The engine plays audio tracks through a `ClipPlayer` node per track into a master `Mix` node; mute/solo are honoured.

**Decisions**
- `AudioData` (planar f32) and `Peaks` live in `musiton-core` so engine, I/O and UI share them without extra dependencies.
- The project → engine graph translation lives in the app (`engine_graph.rs`), because the engine must not depend on the project crate. The graph is rebuilt (and cross-faded) whenever the document revision or the set of loaded audio files changes.
- Clip positions are converted from ticks to samples inside `ClipPlayer::process` using the current tempo map, so tempo changes need no rebuild. Audio clips are not time-stretched.
- New edits: `ReplaceClip`, `MoveClipToTrack`, `AddAsset`, `Batch` (several edits = one undo step, rolled back if one fails).
- Peaks are kept in memory (computed on load/import, ~10 ms per minute of audio); the `peaks/` folder is not used yet — a disk cache only pays off for very long files.
- Importing into an unsaved project first asks for a save location (audio must be copied into the project folder).
- Fades are linear.

**Golden/accuracy test:** `engine_graph::tests::arrangement_plays_back_sample_accurately` — a kick on every beat and a hat on every off-beat at 120 BPM rendered offline with odd callback sizes; every output sample equals the expected sum exactly.

**Known issues**
- If the audio device runs at a different rate than the project (default 48 kHz), clips are played with linear interpolation (lower quality). Choose 48 kHz in Options → Audio settings.
- Missing audio files are reported in the status bar; the relink dialog comes later.
- No rubber-band (box) selection yet.

## Phase 6 — Mixer
**Built:** Mixer tab with a strip per track plus master: fader (−∞…+6 dB, double-click = 0 dB), pan (double-click = centre), mute, solo, solo-safe (buses), stereo peak + RMS meters with falling peak line and clip indicator (click to reset), output routing (master or a bus), sends to buses (level, pre/post fader, add/remove). Engine: `Strip` node (smoothed gain/pan/mute, pre-fader taps, lock-free `Meter`), `SendGain` node.

**Decisions**
- Pan law: constant power, −3 dB in the centre (hard left/right = 0 dB). A centred track therefore plays 3 dB below its file level per strip (track and master).
- Fader, pan, mute/solo and send levels never rebuild the graph: they are sent as `SetParam` commands (20 ms smoothing, no zipper noise). Only structural changes (clips, routing, adding/removing sends/tracks, loaded audio) rebuild it.
- Solo: soloing a track keeps the buses it feeds audible; soloing a bus keeps its sources audible; solo-safe buses (e.g. reverb returns) always play.
- Routing loops are prevented in the UI (only loop-free buses are offered) and skipped again when building the graph.
- Meters: peak = highest sample since the UI last looked (never missed between frames), RMS over ~300 ms. The UI peak line falls at 20 dB/s.
- Metronome bypasses the master fader.

**Acceptance:** `engine_graph::tests::drum_bus_with_send` — drums → drum bus with a −6 dB send to an FX bus; output and the drum-bus/FX/master meters match the levels computed by hand (also with a pre-fader send and the drum fader fully down). A real reverb on the FX bus comes with the built-in effects in phase 10.

**Known issues**
- No meters in the track headers yet (only in the mixer).
- Instrument tracks have a strip but no sound source until phase 9.

## Phase 7 — Audio recording
**Built:** Input device choice (Options → Audio settings, or "None"), per-track input (mono or stereo pair) and monitoring (off / auto = while armed / on) in the mixer strip, arm button "R" in the track header, Rec button and `R` key in the transport (starts playback if stopped), punch in/out range ("Punch" + bars), recording through a disk thread into `audio/<track name>.wav` takes, loop recording (every loop pass becomes its own take; a new take mutes the clips it covers — right-click → Unmute to bring an older take back), latency compensation: "Measure" (plays a click and times it back through input 1) or an estimate from the buffer sizes, plus a manual correction in ms.

**Decisions**
- Input and output are separate cpal streams. The input callback writes into a ring that the output callback reads frame-for-frame; backlog above 2048 frames is skipped to keep latency bounded. Different clocks of two devices drift slowly — use one interface for input and output.
- Audio thread → disk: fixed-size 256-frame chunks carrying their song position (no allocation, ~5 s of buffer per track). A position jump (loop) or a stop ends a take; the disk thread starts a new WAV file. Lost chunks (disk too slow) are counted and shown in red in the status bar.
- Takes are placed `latency` frames earlier (measured round trip, else 2× buffer size, plus the manual correction); if that would start before 0 the first frames are skipped.
- Recording needs a saved project (files go into its `audio/` folder).
- Recorded files are loaded fully into memory for playback, like imported audio (streaming from disk is a later optimisation for very long recordings).
- `ProcessContext` gained `inputs`, `recording` and `punch`.

**Tests:** `musiton-io/tests/recording.rs` (offline, with a simulated input device): sample-exact alignment of a straight take, one take per loop pass, punch in/out, 2 tracks × 60 s with zero lost chunks and no allocation on the audio thread, latency measurement through a simulated 700-frame loopback cable.

**Known issues / needs the owner**
- The 30-minute 2-track test and "recorded clip lines up with the metronome" need real hardware: arm a track, record a few bars clapping on the click, check the alignment; if it is off, use Measure (cable from output to input 1) or the manual correction.
- Input gaps count as dropouts once the input is running.

## Phase 8 — MIDI + piano roll
**Built:** MIDI keyboard input (midir; choose it in Options → Audio settings → MIDI input), live playing into the armed instrument track (or, if none is armed, the selected instrument track), MIDI clips (double-click an instrument track to create one; click a MIDI clip to open it), piano roll in the Editor tab: Draw/Select tools, click/drag to add notes, right-click deletes, drag notes (time + pitch), drag the right edge to resize, rectangle selection, Ctrl+A, Delete, Ctrl+D duplicate, ↑/↓ transpose (Shift = octave), Q / Quantize with strength, velocity lane (drag bars), keyboard preview (click the keys on the left). MIDI recording (Rec with an armed instrument track) in replace or "Overdub" mode, File → Import/Export MIDI file (.mid, type 1). A simple 16-voice sine synth plays instrument tracks until the real polysynth arrives in phase 9.

**Decisions / deviations**
- **Crate direction:** `musiton-devices` depends on `musiton-engine` (devices implement the engine's `AudioNode` trait), not the other way round as sketched in ARCHITECTURE.md.
- No event routing in the graph: a `MidiTrack` node owns its instrument, turns clip notes (tick → sample with the tempo map) and live MIDI into sample-accurate events and calls the instrument. Transport stop/jumps/loop wraps send "all notes off".
- Live MIDI: midir callback → ring → engine reads it at the start of each block (timing resolution = one block, ≤ 5 ms at 256 samples). Preview notes use the same path via a command.
- Recorded MIDI is moved earlier by the output buffer (what you heard) and stored with tick resolution (PPQ 960 = 0.5 ms at 120 BPM). A recording becomes one clip spanning whole bars; replace mode removes older notes that start inside that range.
- "MIDI learn groundwork": the engine reports the last controller moved (shown in the status bar). Mapping controllers to parameters comes with automation (phase 11).
- Lasso selection is a rectangle selection for now.
- Graph rebuilds (e.g. after editing notes) restart the instrument; editing while playing may cut sounding notes. Node reuse across rebuilds is still open (see phase 2 notes).
- Importing a MIDI file into an empty song also takes over its tempo.

**Acceptance:** `engine_graph::tests::recorded_midi_plays_back_identically` plays notes live while recording, turns the recording into a clip, plays it back and compares the audio sample by sample (identical when the notes fall on ticks; otherwise within ±½ tick).

**Known issues**
- Needs the owner's MIDI keyboard test on Windows (no MIDI hardware here).
