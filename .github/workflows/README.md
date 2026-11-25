# .github/workflows/

GitHub Actions workflows. `ci.yml` and `installer.yml` run on every push to `main`; `windows-test.yml` runs after each successful installer build. Results are under **Actions** on GitHub.

| File | What it does |
|---|---|
| `ci.yml` | On Windows and Linux: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace` and a release-mode `cargo check`. A red run means a commit broke formatting, lints or tests. |
| `installer.yml` | On Windows: installs `cargo-packager` and builds the NSIS installer (`cargo packager --release` in `crates/musiton-app`). The setup `.exe` is uploaded as the artifact **Musiton-setup**; download it from the run page. It can also be started by hand ("Run workflow"). |
| `windows-test.yml` | On a fresh Windows machine: takes the **Musiton-portable** build of the last installer run, installs the free plugins Surge XT and Dexed (newest GitHub releases), runs `Musiton.exe --check-plugins` (plugin self-test) and drives the app through `--remote-control` (state, demo song, play, screenshots). Report, screenshots and log are the artifact **windows-test**; the plugin report is also on the run's summary page. The machine has no sound card and no GPU, so levels and sound cannot be checked there. |

A second artifact, **Musiton-portable**, holds `Musiton.exe`, `ffmpeg.exe` and FFmpeg's licence: it runs without installing (no file association, no Start-menu entry).

Before packaging, `installer.yml` downloads the newest stable LGPL Windows build of FFmpeg from [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds) into `target/release/` (`ffmpeg.exe`, `FFMPEG-LICENSE.txt`). The installer and the portable folder ship both, unmodified; Musiton runs `ffmpeg.exe` as a separate program for the video track.

The installer settings (product name, icons, `.musiton` file association) are in `[package.metadata.packager]` of `crates/musiton-app/Cargo.toml`.

Note: `ci.yml` sets `RUSTFLAGS=-D warnings`. This replaces the `x86-64-v2` flag from `.cargo/config.toml` for CI builds; the results are the same.
