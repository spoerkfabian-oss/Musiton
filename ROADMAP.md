# ROADMAP.md

Each phase ends with something the owner can **run and test**. "Done when" = acceptance criteria; all must hold before the next phase. Details of what was built per phase are in [NOTES.md](NOTES.md).

## Status

| Phase | State |
|---|---|
| 0–11 | done |
| 12 — VST3 hosting | built (VST3, CLAP, sandbox, PDC). Its acceptance test needs the owner's Windows PC (Vital, Surge XT, Dexed, Valhalla Supermassive). |
| 13 — Export | done |
| 14 — Polish & release | done. Still needs the owner's test on a fresh Windows PC with the installer. |
| Later list | done except the plugin items, which come with phase 12. |
| 15–19 | planned (owner decision 2026-11-25). |

Work order from here: phase 15 (remote control), then the owner's test of phase 12 (and phase 14's fresh-install check) through it; fixes from those tests; then phases 16–19 in order.

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
- [x] Lanes per track for any parameter (built-in + later VST3), draw/edit points, curves
- [x] Modes: read / write / touch / latch; record knob moves
- [x] Applied at 32-sample sub-blocks
**Done when:** a filter sweep drawn as automation renders identically every time.

## Phase 12 — VST3 hosting
- [x] Decide: own host layer on `vst3` bindings vs `vst3-host` crate (spike + write decision to NOTES.md)
- [x] Out-of-process scanner (`musiton --scan`) with timeout + blacklist; plugin cache JSON; plugin list by vendor/category (Options → Plugins…)
- [x] Load instrument + effect plugins, audio + MIDI events, parameters (incl. automation), state save/restore in project
- [x] Plugin editor in its own native window (HWND), resizable if supported
- [x] Plugin delay compensation (PDC)
**Done when:** Vital, Surge XT, Dexed and Valhalla Supermassive load, play, automate, save and reload correctly; a crashing plugin during scan doesn't affect the DAW.

## Phase 13 — Export
- [x] Offline bounce of full song / loop / selection → WAV (16/24/32f) / FLAC; dither for 16-bit; normalize option
- [x] Stem export (per track/bus); faster than realtime with progress bar
**Done when:** a 32-bit float export (no dither, no normalize) is bit-identical to the offline golden render of the same project.

## Phase 14 — Polish & release
- [x] Settings page, themes, full shortcut list, tooltips
- [x] Crash recovery from autosave; error dialogs instead of panics
- [x] Performance pass (profile with 32 tracks + plugins on the test PC); optional: parallel graph processing
  *(measured with 36 built-in synth tracks + effects in the cloud: ~39 % of one core; the run with plugins on the test PC follows phase 12)*
- [x] Windows installer via `cargo-packager`, `.musiton` file association, app icon
**Done when:** fresh Windows install → installer → open demo project → plays without dropouts.

## Later (after v1 features, before or with phase 12)
Order decided by the owner. Tick when done.

- [x] Tempo ramps (linear tempo changes between tempo points) and a tempo lane to edit them
- [x] MIDI CC lanes in the piano roll
- [x] Step sequencer
- [x] Sidechain routing UI (the compressor already has sidechain inputs)
- [x] Comping (choose the best parts of several takes)
- [x] Time-stretch / warp for audio clips
- [x] Video track (FFmpeg, shipped as an LGPL `ffmpeg.exe`)
- [x] With phase 12: CLAP hosting (`clack-host`), out-of-process plugin sandbox (per plugin, optional)

Dropped by owner decision: ASIO (proprietary SDK), LV2 (almost only on Linux). WASAPI stays the Windows backend.

## Phase 15 — Remote control (for testing on the owner's PC)
Owner decision (2026-11-25): first of the new phases, so Claude can test Musiton on the owner's Windows PC.
- [x] `musiton --remote-control`: local control port (127.0.0.1 only, off by default, token in the data folder)
- [x] `musiton --remote <command>`: client that sends one command and prints the answer
- [x] Commands: state (tracks, devices, transport, levels, dropouts, status, plugin failures), open/save/new, play/stop/locate, mouse clicks/drags, keys and text into the window, screenshot of the window, log tail, quit
- [x] Plugin self-test (`--check-plugins`, built) reachable the same way
**Done when:** from a shell on the owner's PC Claude can open a song, play it, read the levels, click through the UI and see screenshots.

## Phase 16 — Quick wins
Ideas taken from LYNX SHARE (ideas only, no code: it is AGPL).
- [ ] Missing plugins keep a placeholder with their saved settings; they sound again once installed
- [ ] Command search: a shortcut opens a search field for every command and setting
- [ ] Capture automation: turn the knob moves made while playing (without recording) into automation afterwards
- [ ] Export MP3 and OGG
- [ ] Polysynth: unison voices with detune and stereo spread

## Phase 17 — More built-in instruments
- [ ] Wavetable synth
- [ ] FM synth
- [ ] Drum synth (kick, snare, hi-hat … without samples)
- [ ] Electric piano / organ

## Phase 18 — More built-in effects
- [ ] Chorus, flanger, phaser
- [ ] Filter with LFO (auto-filter, wah)
- [ ] Noise gate and transient shaper

## Phase 19 — Bigger features
- [ ] Chord lane under the ruler; the piano roll can follow it
- [ ] Project versions (named snapshots of a song)
- [ ] DAWproject (`.dawproject`) import and export
- [ ] Modulators (LFO, envelope, macro) for any knob, plugins included
- [ ] MIDI controllers: profiles for common controllers and "Link to controller" on any knob
- [ ] Ableton Link (tempo sync with other apps)
