# CLAUDE.md — Rules for this repo

You are building a DAW (digital audio workstation) from scratch in **Rust**.
The owner does not program; you write all the code. Read `ARCHITECTURE.md`, `ROADMAP.md` and `NOTES.md` (owner decisions, open issues) before every session.

## Workflow
- Work phase by phase from `ROADMAP.md`. Never start phase N+1 while phase N's acceptance criteria are unmet. The current order is in the "Status" section of `ROADMAP.md` (owner decision: the "Later" list before phase 12).
- Commit straight to `main` in small, logical commits (Conventional Commits: `feat:`, `fix:`, `refactor:`, `test:`, `docs:`).
- Before every commit: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` must all pass.
- At the end of each phase: tick the boxes in `ROADMAP.md`, add a short entry to `NOTES.md` (what was built, decisions made, known issues), push.
- Only stop to ask the owner when: a decision is irreversible (file format, licence, public API break), a dependency needs a paid licence or SDK download, or something is truly blocking. Otherwise make the sensible choice and record it in `NOTES.md`.
- Explain user-visible results in plain language (what to click to test it), not in code terms.

## Real-time audio rules (non-negotiable)
Code that runs on the audio thread (everything reachable from `AudioNode::process` and the cpal callback) must NOT:
- allocate or free memory (no `Vec::push` past capacity, `Box::new`, `String`, `format!`, `clone()` of heap types, dropping `Arc`s to their last owner)
- lock (`Mutex`, `RwLock`, `parking_lot`), or block on channels
- do I/O (files, println!, logging), syscalls, or call into the UI
- panic: no `unwrap()`/`expect()`/indexing that can go out of bounds; use `get()` and fail silent (output silence)

Use instead: `rtrb` SPSC rings for messages, atomics for scalars, pre-allocated buffers sized in `prepare()`, the engine's GC thread for deferred drops.
- In debug builds the audio callback is wrapped with `assert_no_alloc` — any allocation aborts. Keep it that way.
- Every DSP node gets an offline unit test (render N blocks, compare against expected output with tolerance). Golden renders live in `crates/*/tests/golden/`; regenerate them only for intended sound changes with `MUSITON_BLESS=1 cargo test`.

## Code conventions
- Rust stable, edition 2021, MSRV pinned in `rust-toolchain.toml`.
- Cargo workspace; crate boundaries as in `ARCHITECTURE.md`. `musiton-engine` must never depend on `musiton-ui`.
- `unsafe` only in `musiton-plugin-host` (FFI) and clearly commented `// SAFETY:` blocks.
- Errors: `thiserror` in libraries, `anyhow` only in the app binary.
- Logging: `tracing` (never on the audio thread; push log events into a ring and log from the UI thread).
- Samples are `f32`, internal sample positions `u64`, musical time in ticks `i64` (PPQ = 960).
- IDs are newtypes (`TrackId(u64)`, `ClipId(u64)` …), never raw indices across thread boundaries.
- Public items get doc comments. No dead code, no commented-out code.

## Platform
- Primary target: Windows 10/11 x64 (MSVC toolchain), WASAPI via cpal. Must also compile on Linux (CI).
- Test hardware is modest (4-core i7-3770, GTX 1070 Ti). Keep UI at 60 fps and audio at 256-sample blocks / 48 kHz without dropouts on that machine.

## Documentation
- Keep `README.md` (features, install, build), `ARCHITECTURE.md` (how the code is built, as it is), `ROADMAP.md` (status) and `NOTES.md` (decisions, open issues, phase log) current whenever behaviour, structure or decisions change.
- Every non-code folder has a short `README.md` saying what lives there and how it is produced or used. Update it when its content changes.
- Documentation is written in English. Answers to the owner are in German.

