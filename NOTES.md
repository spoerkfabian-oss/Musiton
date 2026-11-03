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
