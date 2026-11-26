# NOTES.md

This file is the project's memory:
- owner decisions;
- currently open issues;
- a log per phase: what was built, which decisions were made, and the known issues at that time.

Newest entries are at the bottom of the log. Issues that were fixed later are marked **✓ fixed** where they were first noted. The current list is under "Open issues".

## Owner decisions
- Licence: MIT OR Apache-2.0.
- Name: **Musiton**. Crates are named `musiton-*` (instead of `daw-*`). The plugin scanner is the app itself in scanner mode (`musiton --scan <file>`; planned as `musiton-scan`, see phase 12), and project files end in `.musiton` (instead of `.dawproj`).
- Commits go straight to `main`.
- Project layout: the song folder holds the song file plus `audio/`, `peaks/`, `plugins/` and `autosave/` (see phase 3).
- Phase 12 (VST3) is done **last**: after phases 13 and 14 and after the "Later" list ("Zuerst alles andere").
- Later list:
  - ASIO is **not** built (proprietary SDK).
  - The video track **is** built, with FFmpeg (an LGPL `ffmpeg.exe` shipped with the installer and run as a separate program; owner decision, replaces the earlier DLL plan).
  - CLAP hosting and the plugin sandbox come together with VST3. LV2 is **not** built (owner decision: almost only on Linux).
- Plugins (owner decision, phase 12): they run inside Musiton by default; a per-plugin "Run in sandbox" switch runs one in a separate process (crash-safe, costs some CPU and one block of latency). Plugins that failed before go into the sandbox automatically.
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
- The time signature is one value for the whole song (tempo changes and ramps are editable in the tempo lane).
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
- Workspace lints: `unsafe_code = "forbid"` (the plugin-host crate opts out with its own lint table), `missing_docs = "warn"` (→ error in CI via `-D warnings`).
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
- The transport bar is temporary **✓ fixed (phase 4)**. Tempo is a single constant BPM in the UI **✓ fixed (later list: tempo lane)**.
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
- The transport bar only edits a single constant tempo **✓ fixed (later list: tempo lane)**.

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

## Later list — tempo ramps
**Built**
- `TempoPoint` has an optional `ramp` flag: a ramping point glides linearly in BPM over ticks to the next point. The last point never ramps.
- Tempo lane in the arrangement (toolbar "Tempo"): the tempo is drawn as a curve under the ruler. Click adds a tempo change (snapped to the grid), drag moves a point in time and BPM, double-click turns the ramp to the next point on or off (ramp starts are drawn hollow), right-click deletes. The first point stays at the song start.
- The BPM field in the transport bar shows the tempo at the playhead and edits the section the playhead is in (`TempoMap::with_bpm_at`), instead of replacing the whole tempo map.

**Decisions**
- Tick ↔ seconds conversion uses the exact integral of the ramp (`60/(PPQ·k)·ln(…)`) and its inverse, so it stays RT-safe (no allocation) and exactly invertible.
- File format: `ramp` is omitted when false, so files without ramps are unchanged.
- The lane shows 60…180 BPM, or wider when the song's tempos need it. The range is frozen while a point is dragged.
- Audio clips keep their length in seconds unless warped (see "Later list — time-stretch / warp"): their start follows the tempo map, their content does not.

**Tests:** core — `ramp_glides_linearly_between_points`, `editing_the_tempo_at_a_position_changes_only_that_section`, and the tick↔sample round-trip proptest now includes random ramps. Engine — `metronome_follows_a_tempo_ramp` (clicks land within 1.5 samples of the analytic positions). UI — `tempo_lane_adds_points_and_toggles_ramps`. Step-only maps render bit-identically to before (all golden tests unchanged).

**Known issues**
- The time signature is still one value for the whole song.
- A third quick click on a tempo point counts as a triple-click and does not toggle the ramp again.

## Later list — MIDI CC lanes
**Built**
- MIDI clips store controller changes (`MidiClip.cc: Vec<CcEvent>`: tick relative to the clip, controller, value 0–127, channel). Omitted in the file when empty, so older files are unchanged.
- Piano roll: "Lane" chooses what the lane under the notes shows — Velocity, or a controller (Mod wheel, Volume, Pan, Expression, Sustain pedal, any CC number; controllers used in the clip are marked `*`). Drag draws values (one change per grid step, replacing what was there), right-drag erases; one stroke = one undo step.
- Engine: `MidiTrack` plays controller changes sample-accurately. On start or after a jump it "chases" controllers: each one gets the last value set before the position; controllers that clips moved earlier but do not set before it return to their defaults (so a sustain pedal never stays down).
- Instruments: polysynth and sampler react to sustain pedal (CC 64, holds released keys), volume (CC 7) and expression (CC 11, both squared), and the polysynth adds up to ½ semitone of vibrato with the mod wheel (CC 1).
- MIDI recording keeps controller moves (repeated values dropped); replace mode removes the recorded controllers' old changes in the range. .mid import and export include controller changes.

**Tests:** `controller_changes_play_sample_accurately_and_are_chased_after_a_jump` (engine), `sustain_pedal_holds_notes_until_it_is_released`, `volume_controller_scales_the_level_and_mod_wheel_adds_vibrato`, `pedal_edges_and_gain` (devices), `controller_moves_are_recorded_and_repeats_dropped` (app), `export_then_import_round_trips` now with controllers (io), `painting_draws_a_ramp_on_the_grid_and_replaces_what_was_there` and the piano-roll UI test `drawing_in_the_controller_lane_adds_changes_and_right_drag_erases_them`.

**Known issues**
- Pan (CC 10) and other controllers are stored and sent but the built-in instruments ignore them (pan is done by the mixer).
- Pitch bend and aftertouch are not recorded yet.

## Debug pass and Windows .exe
**Found and fixed**
- **The bottom panel grew over the whole window** after switching to the Mixer tab (also with an empty song). The resizable panel adopts the size its content uses, and the mixer's scroll area asked for slightly more each frame. The panel content now gets exactly the panel's area (clipped, the mixer scrolls).
- **Silent exit when the graphics driver cannot run wgpu** (release builds have no console, so nothing was visible). Musiton now restarts itself with the OpenGL renderer (`MUSITON_RENDERER=opengl`, eframe `glow` feature). If that fails too, it shows an error dialog that names the log file.
- **No log in release builds:** everything is now also written to `musiton.log` in the data folder (rewritten at every start), including panics.
- **App icon missing in cross-compiled exes:** `build.rs` now checks the *target* OS, so the MinGW `windres` embeds the icon when building on Linux.

**Windows .exe**
- The installer workflow additionally uploads **Musiton-portable** (`Musiton.exe`, no installation).
- Cross-build on Linux: `x86_64-pc-windows-gnu` with MinGW, stripped to ~29 MB, imports only Windows system DLLs. Smoke-tested under Wine 9: wgpu has no GPU there and fails, the OpenGL fallback starts; the demo song plays with audio (16 % CPU); the mixer, device and editor tabs work.

**Known issues**
- Under Wine without a GPU, wgpu always fails first; the restart costs about a second. On real Windows PCs wgpu (DirectX 12/Vulkan) is expected to work directly.

## Later list — step sequencer
**Built**
- Bottom panel tab "Steps": the MIDI clip that is open in the editor, shown as a grid of rows × steps (1/8, 1/16, 1/32). Click toggles a step; dragging paints (on or off, depending on the first cell); Shift+click accents a step (velocity 127, drawn orange). One stroke = one undo step. The step at the playhead is outlined.
- Rows: for a sampler in drum mode, one row per pad (C2…D#3, named after the pad's sample). For other instruments, two octaves from "Lowest key"; opening a clip moves them to the C below its lowest note.
- Double-clicking a clip of a drum-mode track opens it in "Steps"; other clips open in the piano roll unless "Steps" is already showing.

**Decisions**
- The sequencer edits the clip's ordinary notes (a lit cell = a note starting inside that step; new steps are one step long, velocity 100). No new data type, so playback, export, undo, MIDI export and the piano roll keep working unchanged, and patterns can be refined in the piano roll.
- The number of steps follows the clip length.

**Tests:** `steps_turn_on_and_off_and_accent` (pure functions) and the UI test `clicking_and_dragging_sets_steps_and_shift_click_accents` (click, drag-paint, accent, drag-erase).

**Found while testing:** UI tests behave differently with `cargo test -p musiton-ui` and `cargo test --workspace`. In the workspace build egui gets its default fonts through eframe's features, so text-sized widgets (toolbars) are wider. Tests that probe for positions must not depend on toolbar sizes.

**Known issues**
- Steps have no per-step probability, swing or note length editing (use the piano roll for lengths).
- Melodic rows show two octaves; others need "Lowest key" or scrolling.

## Later list — sidechain routing
**Built**
- Effects with a sidechain input (the compressor) show "Sidechain: Off / <track>" in their rack header. The compressor's "Sidechain" switch is no longer shown; it follows the choice.
- File format: `DeviceSlot.sidechain: Option<TrackId>`, omitted when unset (old files unchanged).
- Graph: the source track's strip output *before the fader* (outputs 2/3, the pre-fader send taps) goes into the effect's inputs 2/3. So a muted or pulled-down kick still ducks the bass.
- `Project::sidechain_ok(target, src)` refuses sources that would close a loop (the target reaching the source through routing, sends or other sidechains). The UI only offers allowed tracks, and the graph builder skips a stored source that has become a loop, instead of failing.

**Tests:** `sidechain_compressor_ducks_the_bass_with_the_kick` — more than 10 dB of ducking during kick hits with the kick fader all the way down, none without a source, and a self-sidechain neither offered nor breaking the build.

**Known issues**
- Only the compressor has a sidechain input; the EQ, gate-style effects and plugins (phase 12) will follow.
- Sidechain sources are not kept audible when the target track is soloed.

## Later list — comping
**Built**
- Toolbar "Takes": every audio track with overlapping recordings gets one row per take under it ("Take 1" = oldest). Pieces where the take plays are bright, the rest dim.
- Drag over a take row: that take plays in the (snapped) range, the others are muted there. Click a piece: the take plays for that piece. Each comp is one undo step.
- Comping splits the clips at the range edges (the right parts continue the audio where the left parts stopped) and puts short fades (`COMP_FADE`, 1/128 note ≈ 15 ms at 120 BPM) on the cuts, so switching takes does not click.

**Decisions**
- No new data type: takes are the audio clips of a track that overlap clips of another recording. Clips of one recording share an audio file, so a take = an asset. Comps are ordinary clip edits (split, mute/unmute), so playback, export and undo need no changes, and comps stay editable by hand.

**Tests:** `comping_a_range_switches_takes_there_only_and_undoes_in_one_step` (exactly one take plays everywhere, fades at the cuts, source offsets continue, undo restores the original), `tracks_without_overlaps_have_no_takes`, UI test `swiping_over_a_take_row_comps_that_range`.

**Known issues**
- Imported files that happen to overlap on a track also appear as takes.
- The cut fades dip the level slightly around each cut (no overlapping crossfade).

## Later list — time-stretch / warp
**Built**
- Right-click an audio clip → "Warp to tempo": the clip's audio follows the song tempo (also tempo changes and ramps) without changing pitch. The BPM field next to it is the file's own tempo; it starts at the song tempo at the clip, so switching warp on changes nothing until the BPM is set. Changing it rescales the clip so it still ends at the same audio. Warped clips show "warp <BPM>" in their title.
- Trim, split, comping and the waveform view use the warp-aware mapping (`AudioClip::source_secs` / `tick_at_source_secs`).
- `musiton-dsp::stretch::wsola`: WSOLA with 2048-frame grains, 4× overlap and a ±384-frame normalized cross-correlation search.

**Decisions**
- Offline rendering instead of a real-time stretcher: each warped clip is rendered on a worker thread into a buffer at the device rate that plays 1:1 from the clip start (`musiton-app/src/warp.rs`). The cache key covers the clip, the tempo map and the rate, so any change re-renders; until the new buffer is ready the clip is silent. Export renders missing buffers synchronously. This keeps the audio thread untouched (no new node, no RT risk).
- File format: `warp` on an audio clip is omitted when off, so older projects are unchanged.

**Tests:** dsp — `stretching_keeps_the_pitch_and_changes_the_length`, `identity_map_reproduces_the_input`, `empty_input_gives_silence`; app — `a_warped_loop_lands_on_the_song_beats` (100 BPM clicks in a 120 BPM song land within 600 samples of each beat), `export_renders_warped_clips_in_time`; ui — `warping_keeps_the_audio_and_a_file_tempo_sets_the_length`.

**Known issues**
- Changing the tempo re-renders every warped clip; long clips fall silent for a moment after each change (render time not yet measured on the test machine).
- No warp markers: one tempo per file. Transients can smear slightly at strong stretch factors (WSOLA, no transient detection).

## Later list — video track
**Built**
- File → Import video…: the video becomes the project's video (one per project) and its sound, if any, is extracted and imported onto a new audio track at the song start. File → Show video window opens the picture.
- The video window shows the frame at the playhead: streamed at 25 fps while playing, single frames while stopped or scrubbing; black before and after the video. "Starts at" shifts the video against the song (one undo step per drag); "Remove video" takes it out.
- `musiton-io::video`: finds `ffmpeg` (`MUSITON_FFMPEG`, next to the executable, `PATH`), probes length/size/sound, decodes scaled-down RGBA frames (at most 640 px wide) over a pipe, extracts the sound as 32-bit float WAV. No console windows on Windows.
- Installer and portable download ship the newest stable LGPL `ffmpeg.exe` from BtbN/FFmpeg-Builds with its licence.

**Decisions**
- Owner decision: run a shipped `ffmpeg.exe` instead of linking FFmpeg DLLs. No build-time FFmpeg, no `unsafe`, and a crash inside FFmpeg cannot take Musiton down. LGPL is met by shipping the unmodified program with its licence.
- The video is referenced by absolute path and not copied into the project folder (videos are large). A missing file shows a message in the video window; nothing else breaks.
- Song time → video time uses the tempo map's seconds, so the picture stays in sync through tempo changes.
- File format: `video` is omitted when there is none.

**Tests:** io — `ffmpeg_info_is_parsed`, `frames_follow_the_requested_time` (generates a red→blue test video, checks still frames, streamed playback order and colours, and sound extraction; skipped when no `ffmpeg` is installed); project — `video_is_saved_and_undoable`.

**Known issues**
- No video lane with thumbnails in the arrangement yet; the start is set numerically in the video window.
- No relink dialog for a moved video; remove and import it again.
- No video export (audio export only).
- `ffmpeg.exe` makes the downloads bigger: the installer is about 49 MB and the zipped portable download about 65 MB (first build with FFmpeg, run #24).

## Phase 12 — plugins
**Built so far**
- `musiton-plugin-host`: finds plugin files in the standard folders (Windows: `Common Files\VST3`, `Common Files\CLAP`, and the per-user `Programs\Common` ones; Linux: `~/.vst3`, `~/.clap`, `/usr/lib/vst3`, `/usr/lib/clap`, `/usr/local/…`) and lists the plugins of a file: VST3 by loading the module and reading its class factory, CLAP through `clack-host`.
- Scanner: Musiton starts itself as `musiton --scan <file>` for each new or changed plugin file. The child prints JSON; a crash, an error or more than 20 s puts the file on the failed list, which is skipped until "Rescan all". Results are cached in `plugins.json` in the data folder; the scan runs in the background at every start.
- Options → Plugins… lists everything found, grouped by maker, with format, instrument/effect and category, plus the failed files and why.
- VST3 instances (`Plugin::load`): host application, component handler, event and parameter-change lists, memory stream and messages are implemented in Rust (`vst3_host.rs`). Setup: stereo buses where accepted, main buses and the first event input active, 32-bit float, real-time mode. Processing passes notes, MIDI CCs the plugin maps to parameters, parameter changes with sample offsets and the transport (tempo, position, time signature). State = component state + controller state (length-prefixed).
- One instance outlives graph rebuilds: each rebuild makes a light `PluginNode`; the newest node claims the instance on its first block and older ones (still cross-fading) output silence, so an instance is never processed twice per block.
- CLAP instances (`clack-host` + `clack-extensions`): audio ports, notes (as CLAP note events and as MIDI), CCs as MIDI, parameter changes (CLAP values are plain; Musiton's 0..1 are mapped through each parameter's range), transport, state, latency and main-thread callbacks. The processor starts on the audio thread; at shutdown it is handed back to the UI thread and deactivated there, as CLAP requires.
- In the app: instrument tracks offer installed instrument plugins in the "Instrument" menu, and "+ Add effect → Plugins → <maker>" lists effect plugins. A plugin device shows a slider per parameter (with the plugin's own value text); plugin parameters appear in the automation lanes ("<plugin>: <parameter>", 0..1) and play back like built-in ones.
- Project format: `DeviceKind::Plugin { format, path, plugin_id, name, instrument }`. The plugin's settings are its own state, written to `plugins/<device id>.state` next to the song file on save, and next to the autosave copy on autosave. Instances live until another song is opened, so undoing the removal of a plugin keeps its settings. A plugin that cannot be loaded shows why in its panel; an instrument then stays silent and an effect is left out of the chain.
- Instances follow the audio device's rate and block size (they are reloaded with their state when these change). An export gets its own instances at the song's rate, created from the current states, so it never touches the live ones.
- Plugin editors (Windows): "Open editor" in the plugin's panel opens its own editor in a native window titled "<plugin> – <track>". VST3 through `IPlugView` with our `IPlugFrame` (size wishes are applied on the next frame), CLAP through the `gui` extension (embedded, `win32`). Resizable editors get a sizing border; user resizes go to the plugin, the plugin's own size changes resize the window. Closing the window detaches the editor. On Linux no editor windows are offered (generic sliders only).
- Delay compensation (PDC): per track, the latency of its instrument and effects (asked after `prepare`, as it can depend on the rate); buses add their own chain to their latest source; every connection into a bus or the master that arrives early goes through a `LatencyDelay`. The click is delayed by the master's latency. The playhead is not shifted: what you hear is late by the total latency (a few ms for typical plugins).
- A bypassed effect still delays by its latency (`FxSlot` keeps a delayed copy of its input), so bypassing does not shift the track against the others. Sidechain sources are delayed to meet the effect's main input when that comes later.
- Found by the PDC test: the built-in limiter reported its look-ahead as latency, but delays one sample less (it writes before it reads). It now reports the real delay; its sound is unchanged.
- Sandbox: "Run in sandbox" in a plugin's panel runs it in its own process (`musiton --plugin-host`). Audio and events go through a shared memory file (`memmap2`); parameters, state and the editor through the child's stdin/stdout as length-prefixed JSON. The audio thread never waits: each block it hands over the input and plays the previous block's result from a FIFO, so the sandbox adds exactly `max_block` frames of latency, which delay compensation evens out. A late or crashed child gives silence; the panel then shows why and offers "Restart plugin". The child's own audio thread spins briefly, then yields, then sleeps 1 ms when idle.
- A plugin that fails to load inside Musiton is tried in the sandbox automatically (owner decision: plugins that failed go into the sandbox).
- The switch is part of the device (`sandbox` in `DeviceKind::Plugin`, omitted when off) and undoable; switching reloads the plugin with its current settings.
- Each plugin library is loaded once per process and shared (`Module::shared`); it exits and unloads when its last instance is gone. Loading/unloading one library twice at the same time crashed the DPF test plugins.
- Loading, scanning and shutting down plugins happen one at a time per process (`lifecycle_lock`). Parallel tests on CI crashed with heap corruption (`free(): invalid size`) when two threads set up or tore down plugins at once; the app only does this on the UI thread anyway.

**Decisions**
- Own host layer on the raw `vst3` bindings (MIT/Apache, generated from Steinberg's SDK headers, which are MIT-licensed since SDK 3.8), not `vst3-host`: Musiton needs real-time-safe processing inside its own engine node, its own parameter/automation path and its own state handling, which a ready-made host would hide. CLAP uses `clack-host`.
- The scanner is the app executable in a special mode instead of a separate `musiton-scan.exe`: one file less to ship and keep in step, and tests can run it via `CARGO_BIN_EXE_musiton`. `tools/scanner` was removed.
- `musiton-plugin-host` has its own lint table (the workspace one minus `unsafe_code = "forbid"`); every `unsafe` block carries a `// SAFETY:` comment.
- Linux CI installs `dpf-plugins-vst3`/`dpf-plugins-clap` (Kars synth, MVerb, …) as test plugins; tests that need them skip when they are missing.

**Tests:** `vst3_and_clap_plugins_are_scanned`, `plugin_files_are_found_by_extension`, `class_ids_are_written_as_hex`, `cache_skips_scanned_and_failed_files_and_survives_a_round_trip`, `hanging_and_crashing_scanners_are_reported`, `scanner_mode_prints_the_plugins_of_a_file`, `a_vst3_synth_plays_notes_and_keeps_its_state`, `a_vst3_effect_processes_its_input_and_newest_node_wins`, `a_clap_synth_plays_and_keeps_its_state`, `a_clap_effect_processes_its_input`, `plugin_states_are_saved_and_reloaded`, `a_plugin_synth_track_renders_its_notes`, `delays_by_exactly_the_given_frames_across_blocks`, `latency_is_compensated_across_tracks`, `a_sandboxed_synth_plays_keeps_its_state_and_survives_a_crash` (kills the plugin process: audio goes on silently without waiting, the panel reports it; this test found a double `RefCell` borrow that would have panicked the UI after a sandbox crash), `messages_and_bytes_survive_the_pipe`.

**Known issues**
- The acceptance test (Vital, Surge XT, Dexed and Valhalla Supermassive load, play, automate, save and reload) needs the owner's Windows PC; the cloud only has Linux test plugins (DPF).
- Autosave also writes plugin states when only a plugin's settings changed (the song's autosave copy must already exist).
- Plugin parameter moves (panel sliders and the plugin's own editor) are recorded into automation lanes in Write/Touch/Latch mode. Touch mode cannot see whether a control in the plugin's own window is still held, so it behaves like a short touch per move.
- The editor windows could not be tried in the cloud (no Windows plugins there); they are compile-checked for Windows only. First real test: on the owner's PC.
- Plugin settings changes are not part of undo (the plugin keeps its own state).
- Sandboxed plugins: each sandboxed plugin keeps one CPU core partly busy while playing; editors of sandboxed plugins open in the child process (Windows), not tested in the cloud.
- Stems of an export share one set of export instances; before each render, 10 s of silence run through them (offline, transport stopped) so tails of the previous stem die away. Tails longer than that would still reach the next stem.

## Owner test (first round, November 2026)
**Reported:** "Instrument 2 behaves oddly"; "instruments only change their sound after switching instruments"; "on Instrument 1 the mixer volume fader cannot be moved, it always jumps to max".

**Findings and fixes**
- Instrument knobs were applied only when the engine graph was rebuilt: the parameter values sent to the engine covered effects and the mixer, but not the instrument. Fixed; test `instrument_knobs_are_sent_without_a_rebuild`.
- Instrument 1 had a volume automation lane in Read mode with one point at maximum (probably a click into the automation lane). A Read lane owns the fader, so every move was undone at once. Now a fader or pan control owned by a Read lane is shown locked with an explanation, and an "Auto" switch under the fader turns those lanes off (the points stay). Test `read_automation_locks_the_fader_and_auto_switches_it_off`.
- Instrument 2 renders cleanly (no invalid values, peak 0.25). Its sound pulses about 1.6 times per second because oscillator 2 is one semitone lower and 20 cents detuned against oscillator 1 on a very low note (C#0): a beating, not a bug.

## Plugin self-test (owner test round 2)
- The owner's Windows PC scans all 8 installed plugins (Dexed, Surge XT, Surge XT Effects, ValhallaVintageVerb, Vital) and Surge XT's editor opens.
- `musiton --check-plugins <report.txt>` checks every installed plugin without anyone listening: load, a note (instrument) or a tone (effect) must give sound, settings are saved and restored, the editor opens and closes, the sandbox plays. Each plugin runs in its own process with a 120 s limit. Meant to be run on the owner's PC through Remote Control.
- Known false alarm: effects that only act on a sidechain signal (e.g. DPF's Amplitude Imposer as VST3) report "no sound".

## Phase 15 — Remote control (2026-11-25)
- Owner decisions: all LYNX SHARE ideas go into the roadmap (phases 16–19, ideas only: LYNX is AGPL); more built-in instruments (wavetable, FM, drum synth, e-piano/organ) and effects (chorus/flanger/phaser, LFO filter, gate/transient shaper); Polysynth unison detune. Remote control comes first so Claude can test on the owner's PC.
- `musiton --remote-control` listens on 127.0.0.1 only, on a random port, and is off unless started with that switch. A random token (from the standard library's randomly keyed hasher) is written with the port to `remote.json` in the data folder; requests without it are refused.
- `musiton --remote '<json>'` is the client (prints one JSON answer; exit code 0 when ok). Positions for clicks are in screenshot pixels. Mouse and key commands answer once their last input step was fed to the window, so a following screenshot shows the result.
- Tested in the cloud on a virtual X display: state, click "+ Instrument", play/stop, locate, screenshot, log, quit.
- `windows-test.yml` (owner decision: test in a VM): GitHub's Windows machines run the plugin self-test with Surge XT and Dexed and a remote-control walk through the app after each installer build. Vital (account needed) and Valhalla are not downloaded automatically. No sound card or GPU there.
- First Windows test results (GitHub's Windows machine): Surge XT and Surge XT Effects (VST3 and CLAP) pass every check: load, sound, settings round trip, editor, sandbox. Dexed passes in-process; in the sandbox its start hung for 30 s. Likely cause: a plugin printing its own text to stdout, which the sandbox used for its messages. Messages now start with a marker and the reader skips foreign text and broken frames. Confirmed: with this, Dexed passes in the sandbox too (all 6 plugin files pass every check). The Windows test now fails when the plugin report has a FAIL line.
- The check's test note now starts after 20 blocks: Surge XT finishes restoring settings in the first blocks and drops notes meanwhile (this looked like "silent after restore").
- The Windows machine has no audio device, so play/levels cannot be tested there.
- Limits: screenshots show Musiton's main window only, not plugin editor windows; native file dialogs cannot be driven (use `open`/`save` with a path).

## Phase 16 — Quick wins (2026-11-25)
- Polysynth unison: "Unison" (1–7 copies per oscillator), "Unison detune" (cents between the outermost copies and the centre) and "Unison width" (stereo spread). Copies start at spread-out phases; the level is scaled by 1/√copies. One copy (the default) renders exactly as before, so old songs and golden renders are unchanged. While the width is 0 one filter per voice runs, otherwise two. New preset "Supersaw".
- MP3 (320 kbit/s, LAME) and Ogg Vorbis (quality 6) export run through the bundled FFmpeg (a temporary 32-bit float WAV is encoded), like video: no encoder is linked into Musiton, so no licence questions. Without FFmpeg the export says so. The Windows test checks that the bundled FFmpeg has both encoders. Known: normalizing to −0.1 dB can make the encoded file overshoot slightly (encoders add tiny peaks).
- Missing plugins: the device stays in the song as before, but its saved settings are now kept in memory and written again on every save (also "Save as" to another folder and autosave). The panel says the plugin is not installed and that its settings are kept. After every plugin scan, devices that did not load are tried again; if the plugin file moved, an installed plugin with the same format and id is used (the song keeps the old path, the lookup repeats on each load).
- Command search: Ctrl+K (or Help → Command search) opens a search over menu commands, editor shortcuts and settings; arrow keys and Enter, or a click. Editor commands are run by pressing their shortcut through the same input path as remote control. Settings entries open the settings or audio dialog; the theme can be set directly.
- Capture automation: knob, fader and plugin moves made while playing on controls whose lane does not record are kept (only the last playback with moves; after a loop jump the newer pass wins). Edit → Capture automation (or the command search) writes them into automation lanes in Read mode as one undo step, creating lanes where needed.

## Phase 17 — More built-in instruments (2026-11-25)
- Owner decisions: ship Surge XT, Surge XT Effects and Dexed with Musiton; besides the planned instruments also plucked strings, a string ensemble, an acid bass and "various synths" (read as: a range of presets per instrument).
- Shipped plugins: `installer.yml` downloads the newest Windows releases into `plugins/` next to `Musiton.exe` (installer and portable build), with Surge XT's factory data as `SurgeXTData` (from the source archive of the same release, since the plugins-only zip has none), the GPL-3.0 text and a README naming the sources. Musiton searches that folder before the system folders. Both are separate programs Musiton loads, so Musiton's own licence is unaffected. Not yet verified that Surge XT finds `SurgeXTData` there for both VST3 and CLAP (it may only look next to the plugin binary); without it Surge XT still works, only without factory presets. A plugin installed both bundled and system-wide shows up twice in the plugin list.
- `voices.rs`: shared voice allocation (free → oldest released → oldest, sustain pedal), parameter smoothing for every continuous knob (one-pole, ~5 ms; whole-number and choice values switch at once) and block splitting at events. All new instruments use it.
- New: FM synth (4 operators with own ADSR, 8 algorithms, feedback on operator 4), wavetable synth (5 tables × 8 frames computed from harmonic spectra at the first `prepare`, 9 band-limited copies per frame, unison up to 5, sub, filter, mod envelope and LFO on position), acid bass (mono, last-note priority, slide on overlapping notes, accent from velocity ≥ 0.75, 24 dB ladder filter added to `musiton-dsp`), drawbar organ (9 drawbars, single-trigger percussion, key click, rotary speaker with spin-up), electric piano (phase-modulated tine with velocity bark, bell partial, key-dependent decay, tremolo), plucked strings (Karplus-Strong, two detuned strings, pick-position comb, three body types), string ensemble (detuned saws at 8' and 4', three-line chorus), drum synth (GM drum keys: kick, snare, clap, three toms, closed/open hat with choke, crash, rim, cowbell). `musiton-dsp` also got `sin_turns`, a fast polynomial sine.
- Tests: every preset of every synth must sound, stay below 0 dBFS, be finite and fall silent 3 s after release; notes must have their pitch; the knob test (no clicks when any knob jumps) covers them; golden renders of the first two presets; wavetable aliasing check.
- CPU (16 voices, this machine, one core): organ about 10 %, wavetable "Sweep pad" (4× unison) about 10 %, the rest 0.3–5 %.
- Known: the piano roll does not yet show drum names for the drum synth's keys; the drum synth ignores note-offs (one-shots).

## Owner report: Bluetooth headphones give no sound (2026-11-25)
- Report: choosing Bluetooth headphones as output gives no sound; going back to the laptop speakers stays silent until Musiton is restarted. Not reproducible in the cloud (no Bluetooth); fixed for the likely causes:
  - The input is on by default with "System default". With Bluetooth headphones connected, Windows often makes their microphone the default input; opening it switches the headphones into phone mode (hands-free profile) and their stereo output goes silent. Musiton now skips a default input that is a Bluetooth headset microphone (name contains "Hands-Free", or the same model name as the output with "Headset"/"Bluetooth") and says why in the status bar; the audio dialog marks such inputs. Choosing one explicitly still works.
  - A sample rate saved for the previous device made the new one fail ("unsupported sample rate"). The output now falls back to the device's own rate, and the dialog resets rate and buffer size that the newly chosen device does not offer.
  - A stream that builds but never asks for audio (device busy, headphones in phone mode) went unnoticed. The output now counts its callbacks; after 3 s without one (or after a driver error) Musiton switches once to the system default output and says so. Input is closed before the output when audio restarts.
- Confirmed by the owner (2026-11-26): Bluetooth headphones work.

## Phase 18 — More effects (2026-11-26)
- Chorus (1–4 voices) and flanger (feedback ±0.95) share one delay-line node (`effects/modulation.rs`); their base delay glides over ~250 ms like tape, since a fast delay change is a pitch jump (click). Phaser: 4–12 first-order all-pass stages, swept ±2 octaves, feedback. Auto filter / wah: SVF (low/high/band-pass) moved by an LFO (sine, triangle, square, random; square and random smoothed over 3 ms) and an envelope follower; cutoff glides in octaves over 30 ms. Gate: peak detector, attack ramp, hold, release, range, optional sidechain (inputs 2/3, like the compressor). Transient shaper: a fast envelope and two followers of it (slow attack → hits, long release → ring-out), up to ±15 dB.
- They use `voices::Params` for knob smoothing. Golden renders with default settings in `tests/effects_more.rs`; the knob test covers them.
- Owner feedback the same night: built-in instruments sound alike and not real; unison/detune never sounds great. Decision: both ship sampled instruments and make the synths more alive (phase 18b). Reference audio comes via Google Drive (videos too; the sound is taken out with FFmpeg).
- Bundled plugins: all six Surge XT/Dexed files pass the Windows plugin check. Checked the other free plugins (2026-11-26): Dragonfly Reverb ships as VST3 only (its CLAP editors would not open in the check); Cardinal ships only "Cardinal Synth" and "Cardinal FX" as VST3 (bundles CardinalSynth.vst3 and CardinalFX.vst3; a filter with a space first matched nothing) (its CLAP builds and the main "Cardinal" gave no sound or no editor in the check). Not shippable: sfizz (its release zip holds only the library, no plugins, and the project is archived; Musiton's own sample instrument plays SFZ), Odin 2 (Windows only as an installer .exe), OB-Xd (freeware, not open source), Firefly Synth (no binary release), Airwindows Consolidated (Windows build only as a VCV Rack plugin), Vitalium (the open-source Vital: no Windows binary in DISTRHO-Ports; Vital itself is a free download with an account). The build cache had kept an old portable folder, so plugins landed in plugins/plugins: both folders now start clean.

## Phase 18b — Reference sound: "raw" just-intonation stabs (2026-11-26)
- The owner sent four TikTok videos (Google Drive). The wanted sound is Aloboi's "Just Raw" sound (e.g. "Want To Love (Just Raw)", "Give Me More (Just Rawer)"), not a genre. First measurement (wrong, see the correction below): "a C#2 power chord of strongly distorted saws, a moving resonance near 1 kHz, chopped 8ths at 170 BPM". Aloboi's own plugin ("Alobend") retunes chords live into just intonation and plays distorted saws through them: the difference tones of a pure chord land on the overtone series instead of beating.
- Polysynth (backward compatible, new params 28–31): Drive (anti-aliased shaper on the summed voices so a chord intermodulates, curves Soft/Hard/Asym/Fold, 0 = exactly clean), Analog drift (default 0.3: slow per-voice pitch wander, a fixed random offset per unison copy, random start phases for silent voices), Just intonation (held notes retuned to ratios over the lowest held note: 3:2, 5:4, 7:4 …, osc 2's interval too; notes slide 30 ms into a new tuning). Unison copies now sit closer to the centre than the outer ones (an even spread beats mechanically). Below 150 Hz the output is mono.
- New presets: "Raw stab", "Raw reese", "Dirty sub".
- Correction (2026-11-26, owner: "closest I got, still sounds completely different"; three agents: measurement, research, code review). Measured again on the exposed synth (oscilloscope video 0–48 s, Aloboi's own Logic video): **clean**, bright, unfiltered saws (flat to 12.7 kHz after the saw slope, no resonance; the "moving resonance" was only an intro filter sweep), no distortion to speak of (crest 10–13 dB, no flat tops, intermodulation −26 … −51 dB), static (no drift/unison beating, 0 cents), voiced root + just fifth + just tenth around 70–260 Hz (C#maj7 C#2 G#2 C#3 F3 C4, F#m, F, F7 with a natural 7:4 seventh), held or gated in sixteenths at 120 BPM (≈ 80 ms on, 40 ms off), side −8 … −23 dB mostly above 2 kHz. Aloboi's own words: "sharp unfiltered saw waves, tuned in Just Intonation with his own plugin"; Alobend picks the root (lowest or most played note), retunes only from a number of held keys, bends single chord notes, and keeps the partials phase-locked (still Lissajous figure). The old "Raw stab" (drive 0.85 → a clipped square-ish wall, low-pass 2.5 kHz with resonance, drift 0.2 which undid just intonation) was wrong on every count, and the owner's test chord (C2 C3 C4: octaves) gives just intonation nothing to do.
- Rebuilt: "Raw stab" = one saw, high-pass 40 Hz, drift 0, drive 0.1 (Asym), sustain 1, just intonation from 2 keys, high width 0.4 — play root, fifth and tenth (C2 G2 E3). The Raw template uses the measured voicings, gated sixteenths at 120 BPM. Polysynth (new params 32–37, older songs unchanged): "Just root" (lowest / most played), "Retune from keys" (fewer held keys keep the last root, so melody notes stay tuned to the chord; the root also stays after all keys are released), "Tuning" (just / overtone series: 17:16, 19:16, 21:16, 11:8, 13:8, 27:16, 7:4 …), "Bend range" and "Bend note" (pitch bend moves the chord or only its n-th note from the bottom), "High width" (the delayed high band of the mid into the side: wide without beating, mono sum unchanged, bass centred). With just intonation a silent voice starts at fixed phases (every stab identical). Bug fixed: notes of a chord that arrived before its root slid 30 ms into tune; notes started within 5 ms now take the tuning at once.
- Pitch bend is new in Musiton: MIDI pitch bend travels as controller 129 (VST3's number for it) through live input, recording, clips (piano-roll lane "Pitch bend", centre 64), MIDI files (written as real pitch bend) and the sandbox; CLAP plugins get real pitch-bend messages. Only the polysynth reacts among the built-in instruments so far.
- Sound change: the default drift makes existing polysynth projects sound slightly different (intended; the old sound is drift 0). Goldens re-blessed.

## Phase 18c — Genre templates (2026-11-26)
- Owner request: own "environments" per genre. Decisions: both project templates and sound packs; sounds synthesized by Musiton plus clean-licence (CC0) samples; every template has an example; genres Raw, Phonk/Tekk, epic orchestral/choir, trap/hip-hop, house/techno, hyperpop/hardstyle.
- `musiton-app/src/templates.rs`: `TEMPLATES` (name, tooltip, builder) and the shared `assemble()` the demo song now uses too. File → New from template and the command search (Ctrl+K, "template") open an untitled song. Each template is 4 bars, looped, built only from built-in instruments; Phonk and Tekk are separate templates.
- New presets: polysynth "Phonk cowbell" (808 cowbell: two squares 540 : 800 Hz through a band-pass, played as a melody), "Phonk 808", "Tekk lead", "Hyper supersaw"; drum synth "Phonk", "Tekk", "House".
- Levels balanced by measurement (no listening in the cloud): tracks −21 … −33 dB RMS, mixes about −18 dB RMS with peaks ≤ −3 dB. The owner should judge the sound; tweaks are cheap.
- Owner clarification: "own environments" means separate Musiton sections, so a Raw song shows no orchestra. Decisions: per song; filtered with a "Show all" escape. `musiton-devices/src/genres.rs` lists per genre the fitting instruments and factory preset names (the sampler, "Init" and user presets always fit); the transport bar has the selector and "Show all"; `device_types()` filters. File format: new optional `genre` field on the project (absent = all sounds; older files load unchanged, no version bump).
- Sound packs (`sound_packs.rs`): 3–8 one-shots per genre rendered with the template builder (one note or chord, held, tail trimmed at −80 dB with a 5 ms fade, normalized, 24-bit WAV) into `<data dir>/Sounds/<genre>/` the first time the browser's "Sounds" button is pressed; made again when `PACK_VERSION` changes; other files there are left alone. Takes about a second.
- Still to do: orchestral/choir with real samples (after the sampled instruments), CC0 sample packs.

## Phase 18b — Sample instrument (SFZ) (2026-11-26)
- New instrument "Sample instrument" (`sfz_player.rs`, type id `sfz`): per note it plays every region whose key and velocity range match; round robin (`seq_length`/`seq_position`, a counter per region), random layers (`lorand`/`hirand`), release samples (`trigger=release`, with the note-on velocity), loop modes (none, one-shot, continuous, sustain) with loop points from the SFZ file, ADSR per region, volume/pan/`amp_veltrack`, `group`/`off_by` (10 ms fade). 64 voices; samples read with cubic interpolation (no mipmaps, so a library takes its plain size in memory as 32-bit float).
- SFZ reader extended for real libraries (checked against the Salamander piano's files: 1121 regions, 16 velocity layers, two tunings): `#include`/`#define` anywhere on a line and in order (names can be redefined between includes), include paths relative to the main file, key switches (`sw_lokey`/`sw_hikey`/`sw_default`/`sw_last`), controller conditions (`loccN`/`hiccN`, start values from `set_ccN`/`set_hdccN`), `lokey=-1` = not played by notes.
- Added for the Karoryfer/Virtuosity libraries (2026-11-26): `ampeg_hold`, `end`, `amp_velcurve_N`, `rt_decay` (release samples quieter the longer the key was held), controller modulation read at note start (`amplitude_oncc`/`amplitude_cc` multiply as depth % × controller, e.g. microphone levels; `volume_oncc`, `offset_oncc`, `ampeg_*_oncc`/`ampeg_*cc` add linearly) and `xfin_*cc`/`xfout_*cc` crossfades (power curve). Layers that come out silent take no voice; 128 voices (Big Rusty Drums plays 16 layers per hi-hat hit). Checked by reading the libraries' SFZ files (all parse; every note tested plays audible layers at the default controller values) — not yet with their samples (no disk space in the cloud session).
- Not supported yet: loop points stored inside WAV files, filters/LFOs/EQ in SFZ, `<curve>` shapes (controller curves are linear), `tune_oncc`/`pan_oncc` (often bipolar curves, so left out rather than played out of tune), `on_loccN` (controller-triggered samples, e.g. pedal noise), pitch bend, `polyphony`. Libraries larger than RAM would need disk streaming (later, if a library needs it).
- File format: optional `sfz` path on built-in devices (absolute; libraries are not copied into the song). Older files load unchanged. The app reads the SFZ and decodes its samples (`assets.rs`, once per path) on a worker thread (`SfzLoader`) when the song opens or "Load SFZ…" is used, so a big library (the Salamander piano: 1121 samples) no longer freezes the window; the track is silent until its samples are in, the status bar says when it is loaded. A missing sample file only drops its regions. Known: exporting while an instrument is still loading leaves that track silent.

## Phase 18b — Sound library downloads (2026-11-26)
- Options → Sound library (`library.rs`): VCSL (CC0, 180+ instruments: timpani, percussion, bells, harp, pianos, organs, winds …), VSCO-2 CE 1.1.0 (CC0 orchestra) and the Salamander Grand Piano V3 (CC BY 3.0, credit Alexander Holm), each as the GitHub zip of its own repository (licences read on the repositories; VCSL's `sfz` branch, VSCO-2's tag 1.1.0, Salamander's `master`). Downloaded on a background thread to `<data dir>/Library/<id>.zip.part` (continued with an HTTP range request where the server allows it, else started over), unpacked to `<data dir>/Library/<id>/` (zip CRC-32 checked per file; a damaged archive fails and nothing is marked installed), `.complete` marks a finished install. Cancel keeps the partial file. GitHub's archive downloads may not report a size or allow resuming; the dialog then shows only the bytes so far.
- Wavetable and strings got the polysynth's "Analog drift" (new knob, default 0.3; `Drift` in `voices.rs`: slow pitch wander ±0.1 semitone, a fixed ±0.05 semitone offset per unison copy/oscillator drawn on every note, random start phases for a silent voice). Songs saved earlier get the default drift, so their wavetable/strings sound slightly livelier; turn the knob to 0 for the old sound. Golden renders regenerated for these two; sound packs re-render (pack version 3).
- More libraries (2026-11-26, licences read on each repository: CC0): Virtuosity Drums, Big Rusty Drums, Black and Blue Basses, Black and Green Guitars, Meatbass (all from github.com/sfzinstruments, default-branch zips). Their sizes are not stated anywhere, so the dialog says so until the download reports one. Not added: Swirly/Unruly Drums (no SFZ program at the top level, Kontakt files), SamsSonor (CC BY-SA), Naked Drums and DRSKit (CC BY 4.0, possible later with credit). The library list shows only .sfz files that no other file `#include`s (these libraries consist of hundreds of fragment files).
- No free choir library with a clean licence was found (VSCO-2 and VCSL have none); the owner's "orchestral/choir" genre has orchestra only for now.
- "Epic orchestral" template (D minor, 90 BPM): violins, celli, basses, horns, timpani, harp from VSCO-2 when installed, otherwise built-in strings, FM brass, drum-synth tom and plucked harp. The sample instrument fits every genre environment.
- Not tested here: real downloads (the sandbox has no access to the archives' hosts and too little disk) — the downloader is tested against a local HTTP server (resume, damaged archive).

## Phase 18c — EDM from the owner's reference songs (2026-11-26)
- The owner sent 11 EDM songs (e.g. Toby Romeo "Fly High", R3HAB/ARTBAT "Dramma", John Summit/Sub Focus "Go Back", Alok "Friday I'm In Luv", Hardwell "Turn Up The Bass") and likes "structure and sound, like Avicii". Measured: 128–148 BPM (mostly 135–140); intro 8 bars, build-up 8–16, drop 16, break 16–32, build-up, drop 16–24, short outro; first drop after 45–60 s; 2:30–3:30 long. Drops: 70–80 % of the energy below 150 Hz, kick ≈ 50 Hz carrying the sub, a sustained bass pumping against it (quietest right after the kick), leads bright (centroid 1.3–1.8 kHz), lows mono, mids/highs wide, very loud (−5 … −9 dBFS RMS, crest ≈ 8 dB). Mostly minor keys. Two directions: melodic/euphoric and harder/faster.
- Built: genre "EDM"; presets EDM bass, Big room lead, Melodic pluck, Hard lead, Riser and the drum kit "Big room"; templates "EDM melodic" (128 BPM, F# minor, piano — the Salamander piano when installed — pluck hook, supersaw chords) and "EDM big room" (140 BPM, A minor, octave lead hook, chord stabs opening in the build-up). Both are 80-bar songs with sections, a snare roll (quarters → 32nds), a noise riser (cutoff and volume automation), crashes on the drops, sidechain compressors keyed by the kick, and a "Master bus" (glue compressor + limiter, every track routed to it). EDM sound pack.
- Compared with the references (same analysis): sub/mid balance close; first version 6 dB too quiet (drops −15 dBFS RMS vs −8). Cause: the bus strip and the master strip each apply the −3 dB centre pan law after the limiter. The master bus now makes that up (+6 dB strip gain), brightens and widens the top (high shelf +4 dB at 5 kHz, width 1.4), soft-clips and limits with a fast release (20 ms, drive 20 dB; with the default 50 ms the limiter ducked the beat and more drive hardly got louder). Also sixteenth hats in the drops, a louder hat in the "Big room" kit, bass −3 dB, harder kick drive. Now: drops −9.0 (melodic) / −8.5 (big room) dBFS RMS, crest 8.7 / 8.2 dB, sub 42–44 %, mids/highs side −4 … −7 dB. Still a little less top end than the references (2–11 kHz 3–4 % vs ≈ 5 %).
- Known: the −3 dB centre pan law on the master strip is unusual (most DAWs leave the master at 0 dB); changing it would change every song's level, so it stays for now.
- The template test now solos each track from its first note (arranged parts start late; a master bus must stay audible).

## Fix: notes after pause/play (2026-11-26)
- Owner: pausing in the middle of a note and pressing play gave no sound until the next note. The MIDI track now chases notes that span the playback start or a jump (note-on at the new position).

## More synth for the owner's EDM songs (2026-11-26)
- Owner: "more synth choice in the polysynth so it sounds like Aloboi / Friday I'm In Luv / Turn Up The Bass". Two agents: web research (patch breakdowns are mostly unpublished; genre recipes) and measurement of five uploaded songs. Measured: everything is saw-based — Fly High 7–9 saws ±30–35 c, very wide; Turn Up The Bass intro riff saws at 0/+12/+19 st without detune, drop supersaw ≈ 50 c and fully wide; Dramma rolling bass a saw through a steep low-pass ≈ 350 Hz; HEADSHOT a dark pad (low-pass ≈ 500 Hz) whose whole chord glides 5–9 st over 250–530 ms, vibrato 3.7 Hz ±5 c; Friday/HEADSHOT a sine sub with soft saturation; all drops pump 5–13 dB (some cut 30–40 ms before the kick). No evidence for pulse/PWM, FM, sync or formant filters, so none were built.
- Owner chose all four: polysynth unison up to 9 voices, oscillator 3 (osc 1's wave at its own interval, default +19), sine sub an octave down, filter slope 12/24 dB (second, non-resonant stage), pitch envelope (±48 st, decay), glide mode "Each voice" (a new note takes the released voice nearest to it; the starting pitches of a chord's new notes are then sorted so voices never cross). New effect "Ducker (pumping)": depth, rate 1/4–1 bar, hold, release, pre-dip; follows the song position while playing and the tempo while stopped. New presets (EDM, some also techno/hyperpop): Big room riff, Euphoric supersaw, Rolling bass, Gliding pad, Sine sub.
- Sound change: with up to 9 unison copies the drift draws more random numbers, so random start phases differ from before (same character); goldens polysynth-init/warm-pad and the app's filter sweep re-blessed.

## Vienna Performer and real-time export (2026-11-26)
- The owner sent VSL's Vienna Performer licence (AI orchestral/vocal renders in the cloud through a plug-in) and installed the free plugins. Licence reading: the owner may use the plug-in in any compatible host (Musiton) and use the renders in their own productions, also commercially; Musiton must not ship it, talk to the VSL service other than through the plug-in, or put renders into sound packs/libraries for others (6.3); every render costs credits and needs an internet connection; buyers must be adults (or a parent's account); AI-use disclosure is the user's duty. Owner decision: support it (cloud plugins) and plan it into templates.
- Plugin check on the owner's PC (24 files): Surge XT, Vital, Valhalla VintageVerb, Cardinal Synth/FX, Dragonfly all pass. Vienna Performer, Vienna Synchron Player (+ Surround) and Vienna Ensemble load and open their editors but give no sound in the check (expected: Performer renders in the cloud, Synchron Player needs installed libraries, Vienna Ensemble is a client of its server app). Dexed (bundled and installed copies, CLAP and VST3) did not finish within 120 s on his PC while it passes in CI; the check now reports the step a timed-out plugin was in.
- Built: export option "Real time" (renders the song part no faster than it plays, for cloud/streaming plugins that fall behind a fast export). The VSL documentation could not be read from the cloud session (host not allowed), so how the plug-in behaves (formats, transport sync, offline bounce, choir) is unknown; the Windows plugin check on the owner's PC will show whether it loads. Template support waits for that.
- Owner's manual test: Vienna Performer connects, but the free account has no instruments ("No instruments for your account yet"), so there is nothing to template yet. Dexed loads and plays (its DX7 factory sounds are what the owner heard as "strange").
- Polysynth "Pure" quick-setting button (next to Presets): switches just intonation on and removes everything that beats (osc 2 detune, unison, drift, pitch LFO) and opens the filter, keeping the other knobs. The owner found that switching on just intonation alone did not give the Raw stab sound. Device types can now carry such quick settings (`DeviceType::quick`).
- Owner sent the transcript of Aloboi's Alobend video: he plays Vital, pitch-bend range 2 in both, and the "raw" sound is the overtone-aligned chord (still Lissajous figure). Alobend features Musiton lacked, now built (owner chose 1/2/3): "Just root" → "Fixed key" with "Fixed root key" (no dynamic retuning), "Bend note counted from" bottom/top, and a ratio choice per interval above the root ("Ratio minor 3rd" = 6/5, 19/16, 7/6, 32/27 …; "Table" keeps the Tuning table). Not built: Alobend's draggable cents view (D-tune mode).
- Found on the way: the engine applies at most 64 commands per block, so offline renders of projects with many parameters started a block late; `render_offline` now applies the whole queue first.
- Listening tests: the owner preferred square + unison + fold (A) and saw+square overdrive wide (C), but "something distinctive is still missing" in every variant.
- The owner recorded 36 s of "Want To Love (Just Slowed Raw)". Measured: plain saws (harmonic fit 5.7 dB error, square 11.5 dB), no unison or detune (single sharp partials), overtone-series chords (27/16, 7/4, 15/8 over the root) from the second octave to about 1 kHz over a sine an octave below the root about as loud as the chord, most energy below 250 Hz, dry and centred synth with about 1.5 s of reverb making all the stereo (side −14 dB), crest factor 7.6 dB. Changed: Raw stab uses the overtone table, release 80 ms, no widener; the Raw template plays those voicings (B – C# – D# – C#) over the "Sine sub" preset with a wetter 1.5 s reverb. Its render now matches the reference's band levels within about 4 dB and its stereo width.
- Owner: the template "sounds very similar, but should be fuller and deeper". Raw stab got a second saw an octave below (pure, no detune) and full velocity response; the template plays the low chord notes loudest (velocity 127 at B1, −2 per semitone). The 60–500 Hz balance moved towards the reference (250–500 Hz from +3.6 to +2.8 dB, 60–120 Hz from −3.7 to −2.3 dB).
- Owner: "deeper and fuller in the original, the rest fits very, very well". Octave-below saw at 0.8, a gentle 8 kHz low-pass instead of the open high-pass, velocity −3 per semitone: every band from 20 Hz to 8 kHz is now within 2.5 dB of the reference.
- Fix: an audio file dropped from the browser onto an instrument track used to become an audio clip covering the MIDI notes; it now becomes the track's sample (sampler), and on a bus or folder it goes to a new audio track.

## Raw sound (owner-approved recipe, 2026-11-26)
The owner approved this sound ("genau so"). Keep it when building anything in the Raw style:
- Synth: polysynth preset "Raw stab": saw, a second pure saw an octave below (level 0.8, no detune), no unison, no drift, overtone-series tuning (just intonation, retune from 2 keys), low-pass at 12 kHz, attack 2 ms, release 80 ms, full velocity response, no drive (owner: "clearer").
- Voicings (`RAW_B`, `RAW_CS`, `RAW_DS`, `RAW_AS` in `templates.rs`): overtone stacks from the second octave to about 1 kHz; low notes loudest (velocity −3 per semitone above B1).
- Under each chord a "Sine sub" an octave below the root, trim −5 dB. The owner wants the fullness at 100–500 Hz, not in the deep sub: a wide EQ bell (+4 dB at 250 Hz, Q 0.6) on the chord track; 120–500 Hz sit about 2 dB above the reference, 20–60 Hz slightly below.
- Dry, centred synth; all stereo from a 1.5 s reverb (mix 0.25).
- Owner: notes should be held, not short, except where they slide from one chord to the next: held chords end a sixteenth-quarter early and glide per voice (0.15–0.4 s) into the next.
- Templates: Raw (sixteenths without gaps, accented by velocity, gliding 60 ms into each new chord; the owner rejected moving upper notes), Raw flow (the owner's mix of slides and ambient: long 1.5-bar and short 2-beat chords in turn, 120 BPM), Raw slides (two beats per chord, 120 BPM), Raw slowed (a bar per chord, half-time drums, 90 BPM), Raw ambient (two bars per chord, no drums, 3 s room, 70 BPM).

## Self-update (2026-11-26)
- Owner decisions: check at every start (plus Help → Check for updates…), every push to `main` is an update, support both portable and installed copies.
- `installer.yml` publishes each `main` build as GitHub release `build-<run number>` (assets `Musiton-portable.zip`, `Musiton-setup.exe`, latest five kept) and compiles the run number in as `MUSITON_BUILD`. The repository is public, so the app reads `releases/latest` without a token.
- `update.rs`: background check → "Update available" window → download (resumable, with progress, reusing the library downloader) → "Restart and update" goes through the unsaved-changes dialog, starts a hidden PowerShell script and closes. The script waits for Musiton's process to end, then `robocopy`s the unpacked portable folder over the old one (merges, retries locked files) and starts Musiton again, or starts the downloaded setup for installed copies (judged by `uninstall.exe` next to the program). Builds without `MUSITON_BUILD` (local, Linux) never update.
- Known limits: a portable folder in a write-protected place (Program Files) cannot be updated in place; the GitHub API allows 60 unauthenticated checks per hour per address.

## Open: CI maintenance
- GitHub warns that `actions/upload-artifact@v4` targets Node.js 20 (deprecated; forced to run on Node.js 24). Move `installer.yml` (and any other workflow using it) to a release of the action that targets Node.js 24 when convenient. Not urgent: the runs still pass.
