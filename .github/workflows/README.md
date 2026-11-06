# .github/workflows/

GitHub Actions workflows. Both run on every push to `main`. Results are under **Actions** on GitHub.

| File | What it does |
|---|---|
| `ci.yml` | On Windows and Linux: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace` and a release-mode `cargo check`. A red run means a commit broke formatting, lints or tests. |
| `installer.yml` | On Windows: installs `cargo-packager` and builds the NSIS installer (`cargo packager --release` in `crates/musiton-app`). The setup `.exe` is uploaded as the artifact **Musiton-setup**; download it from the run page. It can also be started by hand ("Run workflow"). |

The installer settings (product name, icons, `.musiton` file association) are in `[package.metadata.packager]` of `crates/musiton-app/Cargo.toml`.

Note: `ci.yml` sets `RUSTFLAGS=-D warnings`. This replaces the `x86-64-v2` flag from `.cargo/config.toml` for CI builds; the results are the same.
