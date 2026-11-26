# Musiton

A digital audio workstation (DAW) written in Rust. Windows 10/11 first; it also builds and runs on Linux.

**Status:** all phases of the [roadmap](ROADMAP.md) and the "Later" list are built. Plugin hosting (phase 12) still needs its acceptance test on Windows. Decisions and known issues are in [NOTES.md](NOTES.md).

## What it can do

- **Arrangement:** audio, instrument, bus and folder tracks; clips can be moved, trimmed, split, duplicated, faded and given their own gain. Snap grid and rubber-band selection.
- **Tempo:** a tempo lane with tempo changes and gradual ramps; the metronome, MIDI and clip positions follow it.
- **Audio:** import of WAV, FLAC, MP3, OGG and AIFF; recording with punch-in/out, loop takes and latency compensation. Comping: "Takes" shows every recorded take under its track; drag over a take to use it for that range. Warp: right-click a clip → "Warp to tempo" to make loops follow the song tempo (pitch stays).
- **MIDI:** keyboard input, recording (replace or overdub), piano roll with velocity and controller lanes (mod wheel, volume, pan, expression, sustain pedal, any CC) and quantize, and .mid import/export including controllers.
- **Step sequencer:** the "Steps" tab shows the open MIDI clip as a grid (1/8, 1/16 or 1/32 steps); drum-mode samplers get one row per pad; Shift+click accents a step.
- **Plugins:** VST3 and CLAP instruments and effects (Options → Plugins… lists what was found; plugins are scanned in a separate process, so a crashing plugin cannot take Musiton down). Their settings are saved with the song; delays of plugins are compensated; each plugin can run in its own process ("Run in sandbox"), so a crash inside it cannot take Musiton down. Plugin editors open in their own window (Windows). A song whose plugin is not installed keeps the plugin's settings; it plays again once the plugin is installed and scanned.
- **Instruments and effects:**
  - Instruments: a polysynth (3 oscillators + sine sub, "Pure" button for beat-free just intonation, unison up to 9 voices, 12/24 dB filter, pitch envelope, chord glide, analog drift, anti-aliased drive, dynamic just intonation or overtone tuning with root detection, per-note pitch bend, beat-free stereo width), a sampler with a drum mode, a sample instrument that plays SFZ multi-sample libraries (Load SFZ…), an FM synth (4 operators, 8 algorithms), a wavetable synth, an acid bass, a drawbar organ with rotary speaker, an electric piano, plucked strings (guitar, harp, pizzicato), a string ensemble and a drum synth (General MIDI drum keys, no samples). Every instrument has factory presets.
  - Shipped plugins: Surge XT, Surge XT Effects and Dexed come with Musiton (in its `plugins` folder, GPL-3.0).
  - Effects: EQ with spectrum view, compressor, delay, FDN reverb, limiter, saturation, utility, chorus, flanger, phaser, auto filter / wah, gate (with sidechain), transient shaper and a tempo-synced ducker (pumping without sidechain routing).
  - Presets for all of them.
- **Mixer:** faders, pan, mute, solo and solo-safe; sends and buses; meters in the mixer and in the track headers. Sidechain: a compressor can listen to another track (e.g. duck the bass with the kick).
- **Automation:** lanes for volume, pan and every device parameter, with curves; read, write, touch and latch modes. "Capture automation" (Edit menu) turns knob moves made while playing into automation afterwards.
- **Video:** File → Import video… shows the video in a window in sync with the song, and puts its sound on a new audio track. The video's start can be shifted.
- **Export:** WAV 16/24/32f, FLAC 16/24, MP3 (320 kbit/s) and OGG Vorbis (MP3 and OGG use the bundled FFmpeg), with normalize, dither and stems; faster than real time, or in real time for cloud plugins.
- **Command search:** Ctrl+K finds any command, shortcut or setting by typing a few letters.
- **Safety:** autosave, crash recovery, a dialog for relinking missing audio files, and error dialogs instead of crashes.
- **Demo song:** Help → Open demo song.
- **Genre templates:** File → New from template: Raw, Raw slides, Raw slowed, Raw ambient, Phonk, Tekk, Trap, House, Techno, Hyperpop, Hardstyle, Epic orchestral, EDM melodic and EDM big room (the two EDM templates are full songs: intro, build-up with snare roll and riser, drop, break, second build-up and drop, outro; pumping sidechain and a mastering bus). Each opens a ready song with matching instruments, effects, tempo and a 4-bar example of beat and melody.
- **Genre environments:** the selector in the transport bar sets the song's genre (templates do it for you); instrument and preset lists then show only what fits (a Raw song shows no orchestra). "Show all" lists everything again.
- **Sound library:** Options → Sound library downloads free sample libraries from their original sources (VCSL and VSCO-2 orchestra, Virtuosity and Big Rusty drum kits, Black and Blue basses, Black and Green guitars, Meatbass double bass, all CC0; Salamander grand piano, CC BY 3.0), with progress, cancel and continue. Play them with the "Sample instrument" (Load SFZ…); the Epic orchestral template uses VSCO-2 when it is installed.
- **Sound packs:** the browser's "Sounds" button shows one-shot sounds of the song's genre (kick, snare, hats, 808, cowbell, stabs …), made by Musiton's own instruments on first use. Drag them into a track or the sampler.

## Install (Windows)

Every push to `main` builds Windows downloads and publishes them as a GitHub release (`build-<n>`, the five newest are kept).

**Updates:** once installed, Musiton looks for a newer build at every start (and on Help → Check for updates…). "Download update" fetches it; "Restart and update" closes Musiton (asking to save first), then replaces the portable folder's files and starts again, or runs the new setup for an installed copy. Copies built by hand do not update themselves.

First download:

1. On GitHub, open **Actions → Windows installer**.
2. Open the latest green run.
3. Download one of its two artifacts (or take `Musiton-setup.exe` / `Musiton-portable.zip` from the newest release under **Releases**):
   - **Musiton-setup**: an installer. It adds a Start-menu entry and lets double-clicking a `.musiton` project open Musiton.
   - **Musiton-portable**: `Musiton.exe` with `ffmpeg.exe` (for video). Unzip it anywhere, keep both files together and double-click `Musiton.exe`; nothing is installed.

If Musiton does not start, look at `%APPDATA%\Musiton\data\musiton.log`. If the graphics driver cannot run the default renderer, Musiton restarts itself with OpenGL. If that also fails, it shows a message instead of closing silently.

### Building the .exe on Linux

A Windows `.exe` can also be cross-compiled on Linux with MinGW (this is how the cloud test builds are made):

```bash
sudo apt-get install -y mingw-w64
```
```bash
rustup target add x86_64-pc-windows-gnu
```
```bash
cargo build --release -p musiton-app --target x86_64-pc-windows-gnu
```
```bash
x86_64-w64-mingw32-strip -o Musiton.exe target/x86_64-pc-windows-gnu/release/musiton.exe
```

The result needs only DLLs that every Windows has. The official downloads are built natively by GitHub Actions (MSVC).

## Build from source

1. Install Rust with [rustup](https://rustup.rs). On Windows you also need the MSVC toolchain and the Visual Studio 2022 Build Tools. The Rust version is picked automatically from `rust-toolchain.toml`.
2. On Linux, install these system libraries:
   ```bash
   sudo apt-get install -y libasound2-dev libxkbcommon-dev libwayland-dev libgtk-3-dev libudev-dev
   ```
3. Run Musiton from the repository folder:
   ```bash
   cargo run --release
   ```
   Video needs an `ffmpeg` program on the `PATH` (or next to the executable, or named by the `MUSITON_FFMPEG` environment variable). Everything else works without it.
   A debug build (`cargo run`) is optimized enough to play in real time. It also aborts on any memory allocation on the audio thread, which is meant for development.

### Remote control and plugin self-test (for automated tests)

- `musiton --remote-control` starts Musiton with a control port on this computer only (127.0.0.1). `musiton --remote '{"cmd":"state"}'` then sends one command and prints the answer (`--remote @file.json` reads it from a file). Commands: `state`, `open`, `demo`, `save`, `play`, `stop`, `locate`, `click`, `drag`, `hover`, `scroll`, `key`, `text`, `screenshot`, `log`, `quit` (see `crates/musiton-app/src/remote.rs`).
- `musiton --check-plugins report.txt` checks every installed plugin (loads, makes sound, keeps its settings, editor, sandbox) and writes a report.

### Checks before every commit

```bash
cargo fmt --all
```
```bash
cargo clippy --workspace --all-targets -- -D warnings
```
```bash
cargo test --workspace
```

### Useful extras

- Regenerate the golden reference renders after an intended sound change:
  ```bash
  MUSITON_BLESS=1 cargo test --workspace
  ```
- Run the 32-track performance measurement:
  ```bash
  cargo test --release -p musiton-app perf -- --ignored --nocapture
  ```
- Run the CPU benchmarks of the effects and instruments:
  ```bash
  cargo bench -p musiton-devices
  ```
- Build the installer locally (Windows):
  ```bash
  cargo install cargo-packager --locked
  ```
  ```bash
  cd crates/musiton-app && cargo packager --release
  ```

## Repository layout

| Path | What it is |
|---|---|
| `crates/` | The Cargo workspace crates (see [crates/README.md](crates/README.md)). |
| `.github/workflows/` | CI and the installer build (see [.github/workflows/README.md](.github/workflows/README.md)). |
| `.cargo/` | Workspace-wide Cargo settings (see [.cargo/README.md](.cargo/README.md)). |
| `rust-toolchain.toml` | Pinned Rust version. |

## Documentation

| File | For |
|---|---|
| [CLAUDE.md](CLAUDE.md) | Development rules, above all the real-time audio rules. |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Crates, threads, data model, signal flow, file format, testing. |
| [ROADMAP.md](ROADMAP.md) | Phases with acceptance criteria and their status. |
| [NOTES.md](NOTES.md) | Owner decisions, deviations from the architecture, open issues and a per-phase log. |

## Licence

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
