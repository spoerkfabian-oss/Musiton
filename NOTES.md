# NOTES.md

This file is the project's memory:
- owner decisions;
- currently open issues;
- a log per phase: what was built, which decisions were made, and the known issues at that time.

Newest entries are at the bottom of the log. Issues that were fixed later are marked **✓ fixed** where they were first noted. The current list is under "Open issues".

## Owner decisions
- Licence: MIT OR Apache-2.0.
- Name: **Musiton**. Crates are named `musiton-*` (instead of `daw-*`). The scanner binary is `musiton-scan`, and project files end in `.musiton` (instead of `.dawproj`).
- Commits go straight to `main`.
- Project layout: the song folder holds the song file plus `audio/`, `peaks/`, `plugins/` and `autosave/` (see phase 3).
- Phase 12 (VST3) is done **last**: after phases 13 and 14 and after the "Later" list ("Zuerst alles andere").
- Later list:
  - ASIO is **not** built (proprietary SDK).
  - The video track **is** built, with FFmpeg (LGPL DLLs shipped with the installer).
  - CLAP/LV2 hosting and the plugin sandbox come together with VST3.
  - The order is in [ROADMAP.md](ROADMAP.md).
- Documentation is in English. Every non-code folder has a short README.

## Open issues
**Needs the owner's hardware (cannot be checked in the cloud):**
- Audio output by ear: test tone, demo song, no crackle at 256 samples.
- The dropout counter rises on the cloud machine even when idle, because it has no real sound card. Check it on real hardware.
- Recording: 30 minutes of 2 tracks without dropouts; a recorded clip lines up with the metronome (otherwise use Measure or the manual offset).
- MIDI keyboard: live play, recording, and both instruments up to the highest notes.
- Installer on a fresh Windows PC → open the demo song → it plays without dropouts (phase 14 acceptance).

**Known limitations:**
- WASAPI dropout detection is only the "callback too slow" heuristic. Input gaps also count as dropouts.
- Input and output on different devices drift slowly against each other. Use one interface for both.
- Live MIDI timing resolution is one audio block (≤ 5 ms at 256 samples).
- The tempo can only be edited as one constant BPM in the UI. The engine supports tempo maps; tempo ramps are next on the Later list.
- The limiter's 72-sample latency is not compensated yet (PDC comes with phase 12).
- Saturation is not oversampled. In the sampler, a stolen voice can click when all 16 voices are busy.
- Moving a folder into another folder moves only the folder row, not its tracks.
- After undoing a relink, the audio stays loaded until the project is reopened. "Locate…" converts on the UI thread, so long files cause a short freeze.
- Recordings and imports are loaded fully into memory. The `peaks/` disk cache is not used yet.
- Piano-roll "lasso" is a rectangle selection.

## Phase 0 — Foundation
**Built:** Cargo workspace with all crates as stubs, `rust-toolchain.toml` (pinned 1.97.0), CI (Windows + Linux: fmt, clippy -D warnings, test), `tracing` logging in the app (`RUST_LOG` overrides the default `info,wgpu=warn`), empty eframe window titled "Musiton".

**Decisions**
- Library versions at start: egui/eframe 0.37 (wgpu backend), others see `Cargo.toml`. egui 0.37 changed `App::update(ctx)` to `App::ui(ui)`.
- `assert_no_alloc`: the app installs `AllocDisabler` as global allocator in **debug builds only**; audio-thread code is wrapped in `musiton_engine::rt::no_alloc(...)` (no-op in release).
- Workspace lints: `unsafe_code = "forbid"` (the plugin-host crate will opt out in phase 12), `missing_docs = "warn"` (→ error in CI via `-D warnings`).
- Dependencies of external crates are built with `opt-level = 2` in dev builds so debug audio code stays fast enough.
- No top-level `tests/` folder: integration and golden tests live in each crate's own `tests/` folder (engine, devices, app).

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
- The transport bar is temporary **✓ fixed (phase 4)**. Tempo is a single constant BPM in the UI (still open).
- Space toggles play/stop even while a number field is being edited **✓ fixed (phase 4: single-key shortcuts are ignored while typing)**.

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
- The project's `sample_rate` is not linked to the device rate **✓ fixed (follow-ups: background conversion to the device rate)**.
- The transport bar only edits a single constant tempo (still open).

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
- Browser panel and Editor/Mixer tabs are placeholders **✓ fixed (phases 5, 6, 8)**.
- No UI to create or nest folder tracks **✓ fixed (follow-ups)**.

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
- A device rate different from the project rate plays clips with linear interpolation **✓ fixed (follow-ups)**.
- Missing audio files are only reported in the status bar **✓ fixed (follow-ups: relink dialog)**.
- No rubber-band selection **✓ fixed (follow-ups)**.

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
- No meters in the track headers **✓ fixed (follow-ups)**.
- Instrument tracks have no sound source **✓ fixed (phase 9)**.

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
- (Still open.) The 30-minute 2-track test and "recorded clip lines up with the metronome" need real hardware: arm a track, record a few bars clapping on the click, check the alignment; if it is off, use Measure (cable from output to input 1) or the manual correction.
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

## Phase 9 — Built-in instruments
**Built:** **Polysynth** (2 oscillators saw/square/triangle/sine with PolyBLEP anti-aliasing, osc 2 pitch/detune, noise, 12 dB state-variable filter LP/HP/BP with its own ADSR and envelope amount, amp ADSR, LFO to pitch and cutoff, glide, velocity sensitivity, 16 voices with voice stealing) and **Sampler** (load a sample, root key, tune, ADSR, sustain loop with start/end, velocity → volume; drum mode with one sample per pad on keys C2…D#3). New "Device" tab in the bottom panel: choose the instrument of the selected instrument track, knobs for every parameter (drag up/down, Shift = fine, double-click = default), factory presets (Init, Warm pad, Pluck, Bass) and user presets ("Save preset", stored in `%APPDATA%\Musiton\data\presets\<type>\<name>.ron`), "Load sample…"/pad "Load…".

**Decisions**
- `ParamInfo` moved to `musiton-core` (with kind: linear/log/integer/toggle/choice, unit and formatting) so the UI can draw knobs without depending on the engine.
- Projects store devices per track (`devices: Vec<DeviceSlot>`, first = instrument). Only changed parameters are saved; missing ones use the defaults (so new parameters in later versions need no migration). Instrument tracks without a device play the polysynth.
- **No more instrument restarts on edits:** note edits are sent to the running `MidiTrack` node (`EngineCommand::NodeData`, old data freed on the GC thread) and parameter changes via `SetParam`; the graph is only rebuilt when the instrument type, its samples, audio clips or routing change. Notes that were sounding when their clip changed receive a note-off.
- Click-free voices: oscillator phases are never reset and a retriggered/stolen voice starts its attack from its current level. In the sampler a stolen voice restarts its sample (can click when all 16 voices are busy).
- Sampler anti-aliasing: every sample keeps 4 pre-filtered octave levels (63-tap Blackman-windowed sinc), played with cubic interpolation.
- The phase-8 sine placeholder synth was removed.

**Tests:** polysynth — pitch, no aliasing at key 120 (aliases < −30 dB), no clicks when retriggering/stealing/releasing, filter; sampler — pitch tracking, velocity, loop, drum pads, no aliasing two octaves up; DSP — PolyBLEP alias levels, SVF responses and stability under wild modulation, ADSR timing, mipmap filtering.

**Known issues**
- Owner test on real hardware: play both instruments from the keyboard, especially high notes.
- Not every polysynth parameter is smoothed **✓ fixed (phase 10 and follow-ups: levels and resonance)**.

## Phase 10 — Built-in effects
**Built:** Utility (gain, balance, width, phase invert), 8-band parametric EQ (bell, shelves, cuts, notch) with response curve and live spectrum, compressor (threshold, ratio, attack, release, knee, makeup, mix; sidechain inputs prepared), delay (free or tempo-synced incl. dotted/triplet, feedback with high-cut, ping-pong, mix), FDN reverb (8 lines, Hadamard matrix, decay, size, damping, pre-delay, width, mix), look-ahead limiter (ceiling, release, input gain; never exceeds the ceiling), saturation (soft/hard/tube, drive, tone, output, mix). Device rack in the "Device" tab for every track: "+ Add effect", On/off (bypass), `<`/`>` to reorder, `x` to remove, drag an effect's name onto another track's header to move it.

**Decisions**
- Every effect runs inside an `FxSlot` that handles bypass with a 10 ms cross-fade (a fully bypassed effect is not processed). Bypass and parameters are `SetParam` commands; adding/removing/reordering effects rebuilds the graph.
- Track signal path: source (clips/instrument/bus input) + live input → effects → fader/pan/sends.
- All continuous parameters are smoothed (one-pole ~20 ms; biquad coefficients glide; delay time ~80 ms → tape-style pitch glide instead of clicks; reverb size 1.5 s). Envelope sustain now glides too (was a jump in synth and sampler).
- Compressor sidechain: the node has inputs 3/4 and a "Sidechain" switch; choosing a sidechain source in the UI is in "Later" (sidechain routing UI).
- The limiter reports 72 samples of latency; latency compensation (PDC) arrives with phase 12.
- Saturation is not oversampled (some aliasing at high drive on bright material).

**Tests / benches:** golden render per effect (`crates/musiton-devices/tests/golden/*.wav`, tolerance 1e-4), behaviour tests (EQ gain, compressor ratio, delay time + tempo sync, reverb decay ≈ −60 dB/decay time, limiter ceiling + latency, saturation harmonics, utility), a generic **knob test**: for every continuous parameter of every effect and instrument, jumping between min and max every 2048 samples may not create steps larger than the parameter's own steady-state output (this found and fixed clicks in reverb decay/size and envelope sustain). Phase 6 acceptance completed: `drum_bus_with_reverb_send`. CPU (`cargo bench -p musiton-devices`, 1 s of audio, release build in this container): utility 0.17 ms, compressor 1.4 ms, EQ/delay/saturation ~1.8 ms, limiter 2.8 ms, reverb 5.8 ms, polysynth 8 voices 14 ms, sampler 8 voices 20 ms.

**Fixed bugs**
- `cargo build --release` failed: `musiton_engine::rt::AllocDisabler` only exists in debug builds of `assert_no_alloc`. Release builds now use the system allocator under the same name; CI additionally runs `cargo check --release`.

## Phase 11 — Automation
**Built:** Automation lanes per track for volume, pan and every instrument/effect parameter (VST3 parameters will plug into the same `ParamTarget::Device`). Automation view (toolbar "Automation" or `A`): each track shows one lane over its clips; the header chooses the parameter (lanes with points are marked `*`) and the mode. Click adds a point (snapped), drag moves it, right-click deletes, Alt+drag bends the curve between two points. Modes: Off, Read, Write (records the control from playback start until stop), Touch (records while the control is held, then returns to the existing curve), Latch (records from the first touch until stop). Recording takes moves of knobs in the device panel and of mixer fader/pan; each take is one undo step; recorded points are thinned.

**Decisions**
- Points store normalized values (0..1 knob position) with a curve per segment (power curve, −1…1); `curve_shape` lives in `musiton-core` so engine and project share it.
- The engine gets compiled lanes (`EngineCommand::SetAutomation`: node, parameter, scale) and applies them every 32 samples **at song positions that are multiples of 32**, evaluated at the aligned position even when a device buffer splits the block. Together with block-size-independent device internals (polysynth filter updates and reverb updates now use running sample counters) a render no longer depends on the buffer size.
- While a lane is recording, its playback is suspended and the moved control is sent straight to the engine; the static setting is not changed. Lanes in Read mode override the static value during playback and when stopped.
- Track volume automation uses −60…+6 dB.

**Acceptance:** `engine_graph::tests::automated_filter_sweep_renders_identically` — polysynth cutoff sweep drawn as automation (with curves) renders sample-identically with 256-, 100- and 333-sample callbacks and matches `crates/musiton-app/tests/golden/filter_sweep.wav`. Plus recorder tests (touch returns, latch holds, write + thinning) and a UI test (click in automation view adds a point).

**Known issues**
- Only one lane per track is visible **✓ fixed (follow-ups: stacked lanes)**.
- Knobs show the static value, not the automated one **✓ fixed (follow-ups)**.

## Phase 13 — Export
**Built:** File → Export audio…: range (whole song = start to end of the last clip, loop, or selected clips), tail in seconds for reverb/delay, WAV 16/24-bit, WAV 32-bit float, FLAC 16/24-bit, normalize to −0.1 dBFS, TPDF dither for 16-bit, stems (one file per track, named "<song> - <track>.<ext>"). Exports run on a worker thread, faster than real time, with a progress window and Cancel.

**Decisions**
- An export renders the project with the same graph code as playback (metronome off, no loop, automation on) starting from the range start; notes that began before the range start are not played.
- Stems are "solo renders": each track is soloed in turn, so buses it feeds (and solo-safe buses) are included in its stem.
- Dither uses a fixed-seed generator, so exports are reproducible. Normalizing needs the whole render in memory (≈ 23 MB per stereo minute).
- FLAC: `flacenc` (pure Rust). It writes the last (shorter) block size as STREAMINFO minimum; we patch it to the fixed block size as the FLAC spec requires (symphonia refused those files otherwise).

**Acceptance:** `export::tests::float_export_is_bit_identical_to_the_render` — the 32-bit float export of the automated filter-sweep project equals a direct render sample for sample, and `tests/golden/filter_sweep.wav` within 1e-4 (libm differs slightly between Windows and Linux; the first version demanded bit equality with the Linux golden and failed on Windows CI). Also: format round trips (16-bit within dither, 24-bit within ½ LSB, float exact), normalize, stems + cancel.

**Fixed bugs**
- Importing some FLAC files failed: symphonia reports the end of a FLAC stream as an I/O "unexpected end of file"; the decoder now treats that as the end.

## Phase 14 — Polish & release
**Built**
- Options → Settings…: theme (dark/light/system), interface size, autosave interval (0 = off), and a link to the audio/MIDI devices.
- Crash recovery: a lock file (`session.lock` in the data folder) exists while Musiton runs. If it is still there at the next start and the autosave is newer than the saved project, a dialog offers to restore it.
- Error dialogs instead of panics: a panic in the interface is caught. Musiton then makes an emergency save to the autosave file and shows a dialog with a "Quit" button. The lock is kept, so the next start offers the backup. Errors when opening a project now appear in a dialog.
- Help → Open demo song: a generated 8-bar song (drums, bass, pad and lead made with the built-in synth and effects) is written to `Documents/Musiton Demo/` on first use and opened. It needs no audio files.
- App icon: in the window, in the exe (`build.rs` + `winresource`) and in the installer.
- Windows installer: `cargo-packager` (NSIS) config in `crates/musiton-app/Cargo.toml`, with a `.musiton` file association. The GitHub workflow "Windows installer" builds it on every push to `main` and offers it as the download artifact "Musiton-setup".
- Full shortcut list (now including the piano roll) and more tooltips.

**Performance**
- `demo::tests::perf_32_tracks` (ignored; run with `cargo test --release -p musiton-app perf -- --ignored --nocapture`) renders 36 synth tracks with effects (12 reverbs, 6 delays, 6 EQs, 6 compressors). The 2.8 GHz Xeon cloud machine used about 39 % of one core for real-time playback, so the i7-3770 should be in the same range.
- Profiling (callgrind): reverb ≈ 25 %, polysynth ≈ 25 %, EQ ≈ 6 %. Integer divisions and `floorf` library calls were removed from the delay line.
- Builds for x86-64 now target `x86-64-v2` (`.cargo/config.toml`). Every Windows 11 PC supports it.
- Parallel graph processing was not needed at this load and was not built.

**Known issues**
- Two Musiton windows can make the second one offer a wrong recovery **✓ fixed (follow-ups: one lock per window)**.
- A panic on the audio thread is not caught **✓ fixed for release builds (follow-ups)**. Debug builds still abort, on purpose.
- The installer has not been tested on a fresh Windows PC by the owner yet (still open).

## Follow-ups after phase 14 (before VST3)
**Fixed known issues**
- Session locks are per window: each running Musiton holds an OS-locked file in `sessions/`, so a second window no longer triggers a false recovery (uses `File::try_lock`, Rust 1.89+).
- Device knobs (green when automated) and mixer volume/pan show the automated value at the playhead. Moving an automated control still edits the static value, which only matters once the lane is set to Off.
- Stacked automation lanes: in automation view, every lane with points gets its own row under its track. "Show" puts that lane over the clips.
- Polysynth: oscillator/noise levels and resonance are smoothed (20 ms).
- Device rate ≠ project rate: audio files are converted in the background with the high-quality resampler. The graph then switches to the converted copies; exports use the originals.
- Missing audio files: a dialog lists them, with "Locate…" or "Search folder…" (finds files by name, up to 6 levels deep). The found file is copied into the project (`Edit::RelinkAsset`, undoable).
- Rubber-band selection: drag on empty lane space; Shift/Ctrl adds to the selection.
- Level meters in the track headers (RMS, −60…0 dB).
- Folder tracks: "+ Folder"; new tracks go into the selected folder; track menu "Move into folder / Move out of folder"; tracks are indented by depth; folders open and close; folder mute/solo applies to the tracks inside. File format: optional `parent` field on tracks (old files load unchanged; files saved without folders are unchanged).
- Debug builds are optimized (`opt-level = 1` for our crates): the demo song needs ~16 % CPU instead of >100 %.
- An engine panic on the audio thread (release builds) now outputs silence and reports the device as failed; the status bar offers "Restart audio".
- The demo song is balanced (per-part trim utility, mix peak ≈ −2.5 dBFS).
- Installer config: the file-association role must be lowercase (`editor`).

**Known issues:** see "Open issues" at the top (folder move, relink undo, "Locate…" freeze, dropout counter in the cloud).

## Later list — tempo ramps (in progress)
**Built so far (engine side only):** `TempoPoint` has an optional `ramp` flag. A ramping point glides linearly in BPM over ticks to the next point. Tick ↔ seconds conversion uses the exact integral (`60/(PPQ·k)·ln(…)`) and its inverse, so the conversion stays RT-safe (no allocation) and exactly invertible. The last point never ramps. Files without ramps are unchanged (`ramp` is omitted when false).

**Tests:** `ramp_glides_linearly_between_points` (60→120 BPM over 4 beats takes 4·ln 2 s); the tick↔sample round-trip proptest now includes random ramps. Step-only maps render bit-identically to before (all golden tests unchanged).

**Still to do:** the tempo lane in the arrangement (add/move/delete points, toggle ramp) and a transport BPM field that edits the tempo at the playhead instead of replacing the whole map.

