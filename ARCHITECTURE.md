# ARCHITECTURE.md

How Musiton is built. This describes the code as it is. Parts that are still planned are marked **(planned)**. Deviations from the original plan are explained in [NOTES.md](NOTES.md).

## 1. Goals for v1

- Audio recording and arrangement.
- MIDI with a piano roll.
- A mixer with buses and sends.
- Built-in instruments and effects.
- Automation.
- VST3 hosting **(planned, phase 12)**.
- Offline export.

Windows first; Linux must compile and pass CI.

## 2. Crate layout (Cargo workspace)

```
musiton/
├─ crates/
│  ├─ musiton-core/         # time (ticks, samples, TempoMap, TimeSig), ID newtypes, units (dB, pan law),
│  │                        # AudioData/Peaks, ParamInfo. No dependency on other Musiton crates.
│  ├─ musiton-dsp/          # pure DSP: oscillators (PolyBLEP, noise), SVF + biquad filters, ADSR,
│  │                        # delay line, smoothing, FFT, mipmap resampling. No threads, no I/O.
│  ├─ musiton-engine/       # real-time engine: AudioNode trait, compiled graph, transport, command ring,
│  │                        # GC thread, automation, nodes (clip player, MIDI track, strip, FX slot, …)
│  ├─ musiton-devices/      # built-in instruments (polysynth, sampler) and effects; implement AudioNode
│  ├─ musiton-io/           # cpal audio in/out, midir MIDI, file decode (symphonia), resample (rubato),
│  │                        # recording disk thread, WAV/FLAC export
│  ├─ musiton-project/      # project model (source of truth), RON save/load + migrations, Edit/undo
│  ├─ musiton-ui/           # egui views: arrangement, piano roll, mixer, device panel, shortcuts
│  ├─ musiton-plugin-host/  # (planned) VST3 hosting; the only crate allowed to use `unsafe`
│  └─ musiton-app/          # the `musiton` binary: wires everything; settings, autosave, crash recovery,
│                           # project → engine graph translation, export, demo song, installer config
└─ tools/scanner/           # (planned) musiton-scan: out-of-process VST3 scanner
```

Dependency direction (arrows point to dependencies):

```
dsp → core          engine → core, dsp        devices → core, dsp, engine
io → core, engine   project → core            ui → core, project
app → core, engine, io, project, ui, devices
```

- `musiton-engine` never depends on `musiton-ui` or `musiton-project`. The app translates the project into engine graphs (`musiton-app/src/engine_graph.rs`).
- `musiton-devices` depends on the engine because devices implement its `AudioNode` trait.
- Integration and golden tests live in each crate's own `tests/` folder. There is no top-level `tests/` folder.

## 3. Libraries

| Purpose | Crate | Note |
|---|---|---|
| Audio I/O | `cpal` | WASAPI on Windows, ALSA on Linux. ASIO will not be supported (owner decision). |
| MIDI I/O | `midir` | |
| MIDI files | `midly` | .mid import/export |
| GUI | `egui` + `eframe` (wgpu backend) | immediate mode; custom painters for timeline, piano roll, meters |
| File dialogs | `rfd` | |
| Decoding | `symphonia` | WAV, FLAC, MP3, OGG, AIFF |
| Writing | `hound` (WAV), `flacenc` (FLAC) | |
| Resampling | `rubato` + `audioadapter-buffers` | import at project rate; background conversion to the device rate |
| RT messaging | `rtrb` | SPSC rings (commands, live MIDI, recording chunks, GC) |
| Deferred drop | own GC thread | replaced graphs and data are freed off the audio thread |
| RT checks | `assert_no_alloc` | debug builds only |
| Serialization | `serde` + `ron` (project), `serde_json` (settings, session locks) | |
| Paths | `directories` | config and data folders per OS |
| Errors/logs | `thiserror` (libraries), `anyhow` (app), `tracing` | |
| Tests | `proptest`, `criterion`, `approx` | UI tests drive `egui::Context::run_ui` directly |
| Packaging | `cargo-packager` (NSIS), `winresource` (exe icon) | |
| VST3 | **(planned)** `vst3` bindings or `vst3-host` | decided in phase 12, behind our own trait |
| Video | **(planned)** FFmpeg (LGPL, shipped as DLLs) | owner decision for the video track |

## 4. Threads

| Thread | Does | Talks via |
|---|---|---|
| UI (main) | egui, owns the `Project` and its undo stack, builds engine graphs | `EngineCommand` ring → audio; reads `EngineShared` atomics |
| Audio output (cpal callback) | runs the engine block by block; catches panics (release) and outputs silence | rings + atomics only |
| Audio input (cpal callback) | copies input frames into a ring read by the output callback | ring |
| Disk (recording) | writes recorded chunks into WAV takes | ring from the audio thread, channel to the UI |
| GC | drops objects the audio thread retired (old graphs, tempo maps, node data) | ring from the audio thread |
| Workers (short-lived) | import + decode, rate conversion to the device rate, export | `std::sync::mpsc` channels |
| MIDI input (midir callback) | pushes raw MIDI bytes | ring to the audio thread |
| Scanner (subprocess) | **(planned)** probes VST3 plugins | JSON on stdout / cache file |

There is no `EngineEvent` ring. The UI needs only the latest playhead, CPU load, dropout count, meters and recording status, so these are atomics in `EngineShared` and in the meters.

## 5. Core data structures (simplified)

```rust
// ---- musiton-core ----
pub const PPQ: i64 = 960;
pub struct Tick(pub i64);              // musical position
pub struct SamplePos(pub u64);         // absolute sample position
pub struct TempoMap { /* points + cached seconds per point; binary-search lookups, RT-safe */ }
pub struct TempoPoint { tick: Tick, bpm: f64, ramp: bool } // ramp = linear glide to the next point
pub struct TimeSig { num: u8, den: u8 }                   // one signature per song
pub struct ParamInfo { id, name, min, max, default, kind /* linear/log/int/toggle/choice */, unit }

// ---- musiton-project (UI-side source of truth, serialized as RON) ----
pub struct Project {
    pub version: u32, pub sample_rate: u32,
    pub tempo: TempoMap, pub time_sig: TimeSig,
    pub tracks: Vec<Track>,                 // in display order
    pub master: ChannelStrip,
    pub assets: Vec<Asset>,                 // audio files in the project folder
    pub markers: Vec<Marker>,
    pub loop_range: Option<(Tick, Tick)>, pub loop_enabled: bool, pub metronome: bool,
    pub punch_range: Option<(Tick, Tick)>, pub punch_enabled: bool,
    pub next_id: u64,                       // never undone, so ids are never reused
}
pub enum TrackKind { Audio, Instrument, Bus, Folder }
pub struct Track {
    pub id: TrackId, pub name: String, pub color: [u8; 3], pub kind: TrackKind,
    pub clips: Vec<Clip>,
    pub strip: ChannelStrip,
    pub output: OutputRoute,                // Master | Bus(TrackId)
    pub record: RecordSetup,                // armed, monitor (Off/Auto/On), input (Mono/Stereo)
    pub devices: Vec<DeviceSlot>,           // instrument first (instrument tracks), then effects
    pub automation: Vec<AutomationLane>,
    pub parent: Option<TrackId>,            // folder this track sits in
}
pub struct ChannelStrip { gain_db, pan, mute, solo, solo_safe, sends: Vec<SendRoute> }
pub struct SendRoute { target: TrackId, level_db: f32, pre_fader: bool }
pub enum Clip { Audio(AudioClip), Midi(MidiClip) }
pub struct AudioClip { id, asset, start: Tick, length: Tick, source_offset, gain_db, fade_in, fade_out, muted }
pub struct MidiClip  { id, start: Tick, length: Tick, notes: Vec<Note> }
pub struct Note { pitch: u8, velocity: u8, start: Tick /* relative to clip */, length: Tick, channel: u8 }
pub struct DeviceSlot { id: DeviceId, bypass: bool, kind: DeviceKind }
pub enum DeviceKind { Builtin { type_id: String, params: Vec<(u64, f32)> /* only changed ones */, samples } }
                    // Vst3 { … } comes with phase 12
pub struct AutomationLane { target: ParamTarget /* Volume | Pan | Device{device,param} */,
                            points: Vec<AutoPoint>, mode: AutoMode /* Off/Read/Write/Touch/Latch */ }
pub struct AutoPoint { tick: Tick, value: f32 /* normalized 0..1 */, curve: f32 /* −1..1 */ }

// Undo: every change is an `Edit` enum value. "Set" edits swap the stored value with the
// project's (apply = revert); add/remove edits carry the moved object; `Batch` = one step.
pub enum Edit { AddTrack{..}, RemoveTrack{..}, MoveTrack{..}, SetStrip{..}, AddClip{..},
                SetAutomation{..}, SetParent{..}, RelinkAsset{..}, Batch(Vec<Edit>), /* … */ }

// ---- musiton-engine (audio side) ----
pub struct ProcessContext<'a> {
    pub sample_rate: f32, pub frames: usize, pub playing: bool, pub sample_pos: u64,
    pub tempo: &'a TempoMap, pub time_sig: TimeSig, pub metronome: bool,
    pub inputs: &'a [Vec<f32>], pub recording: bool, pub punch: Option<(u64, u64)>,
    pub live_midi: &'a [Event],
}
pub struct Event { pub offset: u32 /* sample within block */, pub kind: EventKind }
pub enum EventKind { NoteOn { ch, key, vel }, NoteOff { ch, key }, Cc { ch, cc, val } }
pub trait AudioNode: Send {
    fn num_inputs(&self) -> usize; fn num_outputs(&self) -> usize;
    fn prepare(&mut self, sample_rate: f32, max_block: usize);   // allocate here, never in process
    fn process(&mut self, ctx: &ProcessContext, io: &mut AudioIo, events: &[Event]);
    fn reset(&mut self);
    fn latency(&self) -> u32 { 0 }                               // for PDC (planned)
    fn params(&self) -> &[ParamInfo] { &[] }
    fn set_param(&mut self, id: ParamId, value: f32) {}
    fn receive(&mut self, data: Box<dyn Any + Send>) -> Option<Box<dyn Any + Send>>; // NodeData swap
}
pub enum EngineCommand { Play, Stop, Locate(SamplePos), SetLoop(..), SetTempoMap(Box<TempoMap>),
    SetTimeSig(TimeSig), SetMetronome(bool), SetInput(..), SetRecording(bool), SetPunch(..),
    SetMidiInput(..), PreviewNote{..}, SetAutomation(..), SwapGraph(Box<CompiledGraph>),
    NodeData{..}, SetParam{ node, id, val } }
```

**Key pattern:**
1. The UI edits the `Project`.
2. The app compares the project's structure with the running graph:
   - **Structural changes** (audio clips, routing, sends, record setup, instrument type or samples, effect chains) build a new `CompiledGraph`. It goes over the ring as `SwapGraph`, and the engine cross-fades old and new for 10 ms. The old graph goes to the GC thread.
   - **Parameter changes** (fader, pan, mute/solo, device parameters, bypass) are `SetParam` commands.
   - **Note edits** are `NodeData` commands. They swap data inside the running `MidiTrack` node, so instruments keep sounding.

## 6. Signal flow per block

1. Drain the command ring.
2. Advance the transport. Split the block at loop boundaries and at 32-sample automation points aligned to the song position. Automation is evaluated at `pos − pos % 32`, so a render does not depend on the device buffer size.
3. Run the graph in topological order:
   1. **Per track:** clip audio, or MIDI events into the instrument.
   2. **Effects:** each in an `FxSlot`, which gives click-free bypass.
   3. **Strip:** fader, pan and mute/solo.
   4. **Sends.**
   5. **Output:** to its bus or to master.
4. Buses run in dependency order, then the master strip. The metronome is added after the master fader.
5. Meters are written to atomics, and recorded input goes to the disk ring.
6. **(planned)** Plugin delay compensation: shorter paths are delayed to match the longest-latency path.

## 7. Project file format and data folders

A song is a folder:

```
MySong/
├─ MySong.musiton     # RON text with a `version` field; migrate() per version; double-click opens it
├─ audio/             # imported (converted to the project rate, 32-bit float WAV) and recorded files
├─ autosave/          # MySong.musiton autosave copy
├─ peaks/             # reserved for a waveform cache (peaks are computed in memory today)
└─ plugins/           # (planned) VST3 state per device
```

- Every struct uses `#[serde(default)]`, so new fields need no migration. Optional new fields are left out of the file when unused, so files of older versions stay unchanged.
- A file with a newer format version is refused with a clear message.
- Missing audio files open the relink dialog. They never cause a crash.

Per-user folders (Windows paths shown; Linux uses the XDG equivalents):

| Path | Content |
|---|---|
| `%APPDATA%\Musiton\config\settings.json` | audio/MIDI devices, UI settings, recent projects |
| `%APPDATA%\Musiton\data\autosave\` | autosaves of songs that were never saved |
| `%APPDATA%\Musiton\data\presets\<type>\` | user presets |
| `%APPDATA%\Musiton\data\sessions\` | one OS-locked file per running window; a stale one means a crash → recovery offer |
| `Documents\Musiton Demo\` | the demo song, written on first "Help → Open demo song" |

## 8. Testing

- **Offline renderer:** `musiton_engine::render_offline(engine, frames, channels, callback_frames)` runs exactly the device-callback code without a device. The app-level `export::render_project` builds a project's graph and renders it the same way.
- **Golden renders:** they live in each crate's `tests/golden/` folder as mono or stereo 32-bit float WAV.
  - Tolerances: engine 1e-5, devices 1e-4, app 1e-4. The app tolerance allows for libm differences between Windows and Linux.
  - Regenerate them with `MUSITON_BLESS=1 cargo test`.
- **Real-time safety:** in debug builds the audio callback runs inside `assert_no_alloc`, so any allocation aborts. The recording tests render under it as well.
- **Property tests:** `proptest` for TempoMap round-trips, undo/redo (apply + revert = identity) and serialization round-trips.
- **Benchmarks and performance:**
  - `criterion` benches per effect and instrument: `cargo bench -p musiton-devices`.
  - An ignored 32-track performance test: `demo::tests::perf_32_tracks`.
- **UI tests:** headless egui frames with synthetic pointer and keyboard events, in `crates/musiton-ui/tests/`. They cover adding and deleting tracks, locating, automation points, stacked lanes, rubber-band selection, and 50 tracks under 8 ms per frame.
- **Knob test:** jumping every continuous parameter of every device between min and max must not click.
