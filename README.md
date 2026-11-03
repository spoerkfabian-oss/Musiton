# Musiton

A digital audio workstation (DAW) written in Rust. Windows first, also builds on Linux.

Status: early development — see [ROADMAP.md](ROADMAP.md) for progress and [NOTES.md](NOTES.md) for decisions.

## Build & run

1. Install Rust via [rustup](https://rustup.rs) (on Windows: MSVC toolchain + Visual Studio 2022 Build Tools).
   The correct Rust version is picked automatically from `rust-toolchain.toml`.
2. In this folder run:

```bash
cargo run --release
```

On Linux you also need: `libasound2-dev libxkbcommon-dev libwayland-dev libgtk-3-dev libudev-dev`.

## Docs

- [CLAUDE.md](CLAUDE.md) — development rules (real-time audio rules!)
- [ARCHITECTURE.md](ARCHITECTURE.md) — crates, threads, data model
- [ROADMAP.md](ROADMAP.md) — phases and acceptance criteria
- [NOTES.md](NOTES.md) — decisions, deviations, known issues

## Licence

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
