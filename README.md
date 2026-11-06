# Musiton

A digital audio workstation (DAW) written in Rust. Windows 10/11 first; it also builds and runs on Linux.

**Status:** phases 0–11, 13 and 14 of the [roadmap](ROADMAP.md) are done. Phase 12 (VST3 plugin hosting) and the "Later" list are still open. Decisions and known issues are in [NOTES.md](NOTES.md).

## What it can do

- **Arrangement:** audio, instrument, bus and folder tracks; clips can be moved, trimmed, split, duplicated, faded and given their own gain. Snap grid and rubber-band selection.
- **Audio:** import of WAV, FLAC, MP3, OGG and AIFF; recording with punch-in/out, loop takes and latency compensation.
- **MIDI:** keyboard input, recording (replace or overdub), piano roll with velocity lane and quantize, and .mid import/export.
- **Instruments and effects:**
  - Instruments: a polysynth and a sampler with a drum mode.
  - Effects: EQ with spectrum view, compressor, delay, FDN reverb, limiter, saturation and utility.
  - Presets for all of them.
- **Mixer:** faders, pan, mute, solo and solo-safe; sends and buses; meters in the mixer and in the track headers.
- **Automation:** lanes for volume, pan and every device parameter, with curves; read, write, touch and latch modes.
- **Export:** WAV 16/24/32f and FLAC 16/24, with normalize, dither and stems; faster than real time.
- **Safety:** autosave, crash recovery, a dialog for relinking missing audio files, and error dialogs instead of crashes.
- **Demo song:** Help → Open demo song.

## Install (Windows)

Every push to `main` builds an installer.

1. On GitHub, open **Actions → Windows installer**.
2. Open the latest green run and download the **Musiton-setup** artifact.
3. Unzip it and run the `.exe`.

The installer also registers `.musiton` files, so double-clicking a project opens it in Musiton.

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
   A debug build (`cargo run`) is optimized enough to play in real time. It also aborts on any memory allocation on the audio thread, which is meant for development.

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
| `tools/` | Helper programs that ship with Musiton (see [tools/README.md](tools/README.md)). |
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
