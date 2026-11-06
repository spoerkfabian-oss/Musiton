# .cargo/

Workspace-wide Cargo settings, read automatically by every `cargo` command in this repository.

- `config.toml` builds for the `x86-64-v2` CPU level on x86-64. Every x86-64 CPU since about 2009, and every PC that runs Windows 11, supports it. The compiler can then use fast SSE4.1 rounding instead of library calls in the DSP code.

An explicit `RUSTFLAGS` environment variable overrides this file (CI does that, see `.github/workflows/README.md`).
