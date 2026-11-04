# ROADMAP.md

Each phase ends with something the owner can **run and test**. "Done when" = acceptance criteria; all must hold before the next phase.

## Phase 0 — Foundation
- [x] Cargo workspace with all crates from ARCHITECTURE.md (empty stubs), `rust-toolchain.toml`, `.gitignore`, README; licence: ask owner (suggest MIT OR Apache-2.0)
- [x] GitHub Actions: windows-latest + ubuntu-latest → fmt, clippy -D warnings, test
- [x] `tracing` setup, `assert_no_alloc` wired as debug feature
**Done when:** CI green on both OSes; `cargo run` opens an empty eframe window titled with the app name.

## Phase 1 — Audio out
- [x] cpal backend: list devices, pick output, sample rate, buffer size; settings dialog; persist to settings.json
- [x] Test tone (sine) through the callback; xrun counter; CPU-load meter
- [x] Offline render path (no device) used by tests
**Done when:** you can choose your output device and hear a clean 440 Hz tone with no crackle at 256 samples.

## Phase 2 — Engine core
- [x] `AudioNode` trait, `CompiledGraph`, buffer pool, command/event rings, GC thread
- [x] Transport: play/stop/locate/loop; TempoMap tick↔sample; metronome (click node, accent on beat 1)
- [x] Graph hot-swap without clicks
**Done when:** metronome plays at any BPM, loop works, changing tempo while playing doesn't glitch; golden test for 4 bars at 120 BPM passes.

## Phase 3 — Project model
- [x] `Project` structs, RON save/load, version field + migration hook, undo/redo stack
- [x] New/Open/Save/Save As, recent files, autosave every 2 min
**Done when:** save → close → open restores everything; 100 random edits + undo all = original (proptest).

## Phase 4 — UI shell
- [x] Layout: transport bar (play/stop/rec/loop/BPM/time sig/position), track headers, timeline, bottom panel (editor/mixer tabs), browser side panel
- [x] Timeline: zoom (Ctrl+wheel), scroll, bar/beat ruler, snap grid (1/1 … 1/32, triplets), playhead follow
- [x] Add/delete/rename/reorder/color tracks; keyboard shortcuts table
**Done when:** UI holds 60 fps with 50 empty tracks; all transport controls drive the engine.

## Phase 5 — Audio clips
- [x] Import WAV/FLAC/MP3/OGG (drag & drop + browser), resample to project rate on import, copy into project folder
- [x] Peak cache (multi-resolution) + waveform drawing
- [x] Clip move, trim both edges, split, duplicate, delete, fades in/out, clip gain; multi-select
**Done when:** you can build a loop-based arrangement from samples and it plays back sample-accurately (golden test).

## Phase 6 — Mixer
- [x] Channel strips: fader (dB), pan (−3 dB law), mute, solo (incl. solo-safe for buses), peak + RMS meters, clip indicator
- [x] Bus tracks, sends (pre/post), output routing, master strip; cycle detection in routing
**Done when:** a drum bus with reverb send works; meters match offline-computed levels.

## Phase 7 — Audio recording
- [x] Input device/channel selection per track, arm, input monitoring (off/auto/on)
- [x] Record to WAV via disk thread (no RT allocations), punch in/out, loop recording creates takes
- [x] Recording latency compensation (measured/manual offset)
**Done when:** 30 min recording of 2 tracks has zero dropouts; recorded clip lines up with the metronome.

## Phase 8 — MIDI + piano roll
- [x] midir input, device selection, MIDI-learn groundwork; live play to armed instrument track
- [x] MIDI clips; piano roll: draw/erase, select (box/lasso), move, resize, duplicate, velocity lane, quantize (+ strength), transpose, keyboard preview
- [x] MIDI recording (replace/overdub), .mid import/export (midly)
**Done when:** you can play a keyboard into a track, edit it in the piano roll and hear it back identically.

## Phase 9 — Built-in instruments
- [x] Polysynth: 2 osc (saw/square/tri/sine, PolyBLEP), noise, SVF filter (LP/HP/BP) with env, amp ADSR, LFO, 16 voices, voice stealing, glide
- [x] Sampler: load sample, root key, ADSR, loop points, velocity → volume; simple drum mode (pad per note)
- [x] Generic parameter UI (knobs) + preset save/load
**Done when:** both instruments are playable from the keyboard without clicks/aliasing artifacts at high notes.

## Phase 10 — Built-in effects
- [x] Utility (gain/pan/phase), 8-band parametric EQ with spectrum view, compressor (sidechain-ready), delay (tempo-sync), reverb (FDN), limiter, saturation
- [x] Device rack: add/remove/reorder/bypass, drag between tracks
- [x] Parameter smoothing everywhere (no zipper noise)
**Done when:** each effect has a golden test and a CPU bench; dragging any knob while playing is click-free.

## Phase 11 — Automation
- [ ] Lanes per track for any parameter (built-in + later VST3), draw/edit points, curves
- [ ] Modes: read / write / touch / latch; record knob moves
- [ ] Applied at 32-sample sub-blocks
**Done when:** a filter sweep drawn as automation renders identically every time.

## Phase 12 — VST3 hosting
- [ ] Decide: own host layer on `vst3` bindings vs `vst3-host` crate (spike + write decision to NOTES.md)
- [ ] Out-of-process scanner `musiton-scan.exe` with timeout + blacklist; plugin cache JSON; browser lists plugins by vendor/category
- [ ] Load instrument + effect plugins, audio + MIDI events, parameters (incl. automation), state save/restore in project
- [ ] Plugin editor in its own native window (HWND), resizable if supported
- [ ] Plugin delay compensation (PDC)
**Done when:** Vital, Surge XT, Dexed and Valhalla Supermassive load, play, automate, save and reload correctly; a crashing plugin during scan doesn't affect the DAW.

## Phase 13 — Export
- [ ] Offline bounce of full song / loop / selection → WAV (16/24/32f) / FLAC; dither for 16-bit; normalize option
- [ ] Stem export (per track/bus); faster than realtime with progress bar
**Done when:** a 32-bit float export (no dither, no normalize) is bit-identical to the offline golden render of the same project.

## Phase 14 — Polish & release
- [ ] Settings page, themes, full shortcut list, tooltips
- [ ] Crash recovery from autosave; error dialogs instead of panics
- [ ] Performance pass (profile with 32 tracks + plugins on the test PC); optional: parallel graph processing
- [ ] Windows installer via `cargo-packager`, `.musiton` file association, app icon
**Done when:** fresh Windows install → installer → open demo project → plays without dropouts.

## Later (not v1)
CLAP hosting (`clack-host`), ASIO, time-stretch/warp, comping, sidechain routing UI, tempo ramps, MIDI CC lanes in piano roll, step sequencer, LV2 on Linux, out-of-process plugin sandbox, video track.
