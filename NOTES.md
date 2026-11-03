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
