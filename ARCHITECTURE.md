# ARCHITECTURE.md

## 1. Goals for v1
Audio recording + arrangement, MIDI + piano roll, mixer with buses/sends, built-in instruments and effects, automation, VST3 hosting, offline export. Windows first.

## 2. Crate layout (Cargo workspace)
```
musiton/
├─ crates/
│  ├─ musiton-core/         # time (ticks/samples, TempoMap), IDs, units (dB, pan law), shared types. No deps on other crates.
│  ├─ musiton-project/      # Project model (source of truth), serde save/load, migrations, undo/redo (commands)
│  ├─ musiton-dsp/          # Pure DSP: oscillators, filters (biquad/SVF), envelopes, delay lines, smoothing. No threads, no I/O.
│  ├─ musiton-engine/       # Real-time engine: AudioNode trait, compiled graph, transport, mixer, clip players, PDC, metronome
│  ├─ musiton-devices/      # Built-in instruments + effects (implement AudioNode, use musiton-dsp)
│  ├─ musiton-io/           # cpal audio backend, midir MIDI I/O, file decode (symphonia), WAV/FLAC write, disk thread, peak cache
│  ├─ musiton-plugin-host/  # VST3 hosting (FFI), scanner subprocess, plugin editor windows. Only crate with significant `unsafe`.
│  ├─ musiton-ui/           # egui widgets: timeline, piano roll, mixer, device rack, browser, automation lanes
│  └─ musiton-app/          # Binary: wires everything, settings, autosave, crash recovery
├─ tools/scanner/       # musiton-scan.exe: out-of-process VST3 scanner (a crashing plugin can't kill the DAW)
└─ tests/               # integration + golden-audio tests (offline render)
```
Dependency direction: `core ← dsp ← devices ← engine ← io/plugin-host ← app`, `core ← project ← ui ← app`. Engine never sees UI types.

## 3. Libraries
| Purpose | Crate | Note |
|---|---|---|
| Audio I/O | `cpal` | WASAPI default; `asio` feature later (needs ASIO SDK + LLVM) |
| MIDI I/O | `midir` | |
| MIDI files | `midly` | import/export .mid |
| GUI | `egui` + `eframe` (wgpu backend) | immediate mode; custom painters for timeline/piano roll |
| UI tests | `egui_kittest` | snapshot/interaction tests |
| File dialogs | `rfd` | |
| Decoding | `symphonia` | WAV, FLAC, MP3, OGG |
| Writing | `hound` (WAV), `flacenc` (FLAC) | |
| Resampling | `rubato` | import at project rate |
| RT messaging | `rtrb` | SPSC ring buffers |
| Deferred drop | `basedrop` or own GC thread | drop old graphs/buffers off the audio thread |
| Atomic swap | `arc-swap` | read-mostly shared data (e.g. tempo map snapshot) |
| RT checks | `assert_no_alloc` | debug builds only |
| Serialization | `serde` + `ron` (project), `serde_json` (settings) | |
| VST3 | `vst3` (bindings) — evaluate `vst3-host` in phase 12 | `vst3-host` is mostly macOS-tested and "not yet lock-free" per its docs → wrap behind own trait either way |
| Errors/logs | `thiserror`, `anyhow`, `tracing` | |
| Tests | `proptest`, `criterion`, `approx` | |
| Packaging | `cargo-packager` | Windows installer (NSIS/MSI) |

## 4. Threads
| Thread | Does | Talks via |
|---|---|---|
| UI (main) | egui, owns `Project`, undo stack, builds engine graphs | sends `EngineCommand` → ring; reads `EngineEvent` ← ring |
| Audio (cpal callback) | runs compiled graph block by block | only rings + atomics |
| Disk | streams/records audio, writes WAV during recording | rings to audio thread, channel to UI |
| GC | drops objects the audio thread retired | ring from audio thread |
| Worker pool | decoding, peak files, graph compile, export | channels (non-RT) |
| Scanner (subprocess) | probes VST3 plugins, writes JSON cache | stdout/JSON file |

## 5. Core data structures (sketches, not final)
```rust
// ---- musiton-core ----
pub const PPQ: i64 = 960;
pub struct Tick(pub i64);          // musical position
pub struct SamplePos(pub u64);     // absolute sample position
pub struct TempoMap { points: Vec<TempoPoint> }        // tick ↔ sample conversion, cached cumulative
pub struct TempoPoint { tick: Tick, bpm: f64 }          // v1: step changes only, ramps later
pub struct TimeSigPoint { bar: u32, num: u8, den: u8 }

// ---- musiton-project (UI-side source of truth, serializable) ----
pub struct Project {
    pub version: u32,
    pub sample_rate: u32,
    pub tempo: TempoMap, pub time_sigs: Vec<TimeSigPoint>,
    pub tracks: Vec<Track>,            // ordered as shown
    pub master: ChannelStrip,
    pub assets: AssetPool,             // audio files referenced by clips
    pub markers: Vec<Marker>, pub loop_range: Option<(Tick, Tick)>,
}
pub enum TrackKind { Audio, Instrument, Bus, Folder }
pub struct Track {
    pub id: TrackId, pub name: String, pub color: [u8; 3], pub kind: TrackKind,
    pub clips: Vec<Clip>,
    pub devices: Vec<DeviceSlot>,       // insert chain: instrument first (if Instrument), then effects
    pub strip: ChannelStrip,
    pub automation: Vec<AutomationLane>,
    pub input: InputRoute, pub output: OutputRoute,   // OutputRoute::Master | Bus(TrackId)
    pub armed: bool, pub monitor: MonitorMode,
}
pub struct ChannelStrip { pub gain_db: f32, pub pan: f32, pub mute: bool, pub solo: bool, pub sends: Vec<SendRoute> }
pub struct SendRoute {   // not `Send` — would clash with std::marker::Send pub target: TrackId, pub level_db: f32, pub pre_fader: bool }
pub enum Clip { Audio(AudioClip), Midi(MidiClip) }
pub struct AudioClip { pub id: ClipId, pub asset: AssetId, pub start: Tick, pub length: Tick,
                       pub source_offset: SamplePos, pub gain_db: f32, pub fade_in: Tick, pub fade_out: Tick }
pub struct MidiClip  { pub id: ClipId, pub start: Tick, pub length: Tick, pub notes: Vec<Note>, pub cc: Vec<CcEvent> }
pub struct Note { pub pitch: u8, pub velocity: u8, pub start: Tick /* relative to clip */, pub length: Tick, pub channel: u8 }
pub struct DeviceSlot { pub id: DeviceId, pub bypass: bool, pub kind: DeviceKind }
pub enum DeviceKind { Builtin { type_id: String, params: Vec<(ParamId, f32)> },
                      Vst3 { class_id: [u8; 16], path: PathBuf, state: Vec<u8> /* component+controller state */ } }
pub struct AutomationLane { pub target: ParamTarget, pub points: Vec<AutoPoint>, pub mode: AutoMode }
pub struct AutoPoint { pub tick: Tick, pub value: f32 /* normalized 0..1 */, pub curve: f32 }

// Undo: every edit is a reversible command
pub trait EditCommand { fn apply(&mut self, p: &mut Project); fn revert(&mut self, p: &mut Project); fn label(&self) -> &str; }

// ---- musiton-engine (audio-side) ----
pub struct ProcessContext<'a> {
    pub sample_rate: f32, pub block_len: usize,
    pub transport: &'a TransportState,   // playing, recording, sample_pos, tick_pos, bpm, loop
}
pub struct Event { pub offset: u32 /* sample within block */, pub kind: EventKind }
pub enum EventKind { NoteOn{ch:u8,key:u8,vel:f32}, NoteOff{ch:u8,key:u8}, Cc{ch:u8,cc:u8,val:f32},
                     PitchBend{ch:u8,val:f32}, Param{id:ParamId,val:f32} }
pub trait AudioNode: Send {
    fn prepare(&mut self, sample_rate: f32, max_block: usize);      // allocate here, never in process
    fn process(&mut self, ctx: &ProcessContext, io: &mut AudioIo, events: &[Event]);
    fn reset(&mut self);
    fn latency(&self) -> u32 { 0 }                                  // for PDC
    fn params(&self) -> &[ParamInfo];
}
pub struct CompiledGraph {        // built on UI/worker thread, swapped in atomically
    nodes: Vec<Box<dyn AudioNode>>, order: Vec<NodeIdx>,          // topological order
    buffers: BufferPool,                                            // pre-allocated
    routes: Vec<Route>, pdc: Vec<DelayLine>,
}
pub enum EngineCommand { Play, Stop, Locate(SamplePos), SetLoop(Option<(SamplePos,SamplePos)>),
                         SwapGraph(Box<CompiledGraph>), SetParam{node:NodeIdx,id:ParamId,val:f32},
                         Arm{track:TrackId,on:bool}, LiveMidi(Event), SetMetronome(bool) }
pub enum EngineEvent { Playhead(SamplePos), Meters(MeterFrame), Xrun, RecordedChunk(ChunkId), CpuLoad(f32) }
```
**Key pattern:** the UI edits `Project` → a worker compiles a new `CompiledGraph` (reusing existing node instances where possible so plugins/synths keep state) → `SwapGraph` over the ring → audio thread swaps pointer at block start → old graph goes to GC thread. Small changes (fader, mute, param) go as direct commands without a recompile.

## 6. Signal flow per block
1. Drain command ring (bounded count per block).
2. Advance transport; compute tick range of this block; split block at loop boundary.
3. Per track: gather clip audio / MIDI events in range → instrument → insert FX → automation applied at sub-block resolution (32 samples) → fader/pan → sends → output bus.
4. Buses in dependency order → master → limiter (optional) → output.
5. PDC: delay shorter paths to match the longest-latency path to master.
6. Write meters into atomics / event ring; push recorded input to disk ring.

## 7. Project file format
`MySong.musiton/` folder: `project.ron` (versioned, with `migrate()` per version), `audio/` (imported + recorded files, copied in), `peaks/` (cache, regenerable), `plugins/<device-id>.bin` (VST3 state), `autosave/`. Missing files → "relink" dialog, never a crash.

## 8. Testing
- **Offline renderer** (`musiton_engine::render_offline(project, range) -> Vec<f32>`) is the backbone: same graph code, no device. Golden tests compare against stored reference renders (tolerance 1e-5 or RMS diff).
- `assert_no_alloc` around the callback in debug; a CI test renders 10 s of a heavy project under it.
- `proptest` for TempoMap tick↔sample round-trips, undo/redo (apply+revert = identity), serialization round-trip.
- `criterion` benches for graph process cost per track count.
- UI: `egui_kittest` for core interactions (add track, drag clip, draw note).
