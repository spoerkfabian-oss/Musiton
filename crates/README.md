# crates/

The Cargo workspace crates. Each one is a Rust library except `musiton-app`, which builds the `musiton` program. How they fit together (dependency direction, threads, data model) is in [ARCHITECTURE.md](../ARCHITECTURE.md).

| Crate | Role |
|---|---|
| `musiton-core` | Shared basics: ticks/samples and the tempo map, time signature, ID types, dB/pan units, audio buffers and waveform peaks, parameter descriptions. |
| `musiton-dsp` | Pure signal processing: oscillators, filters, envelopes, delay lines, smoothing, FFT, resampling. No threads, no I/O. |
| `musiton-engine` | The real-time audio engine: node graph, transport, metronome, mixer strips, clip and MIDI playback, automation, command ring and GC thread. |
| `musiton-devices` | Built-in instruments (polysynth, sampler) and effects (EQ, compressor, delay, reverb, limiter, saturation, utility). |
| `musiton-io` | Audio and MIDI devices, reading/converting audio files, the recording disk thread, WAV/FLAC export. |
| `musiton-project` | The project model saved to `.musiton` files, loading/saving with migrations, and undo/redo (`Edit`). |
| `musiton-ui` | The editing views drawn with egui: arrangement, piano roll, mixer, device panel, shortcut list. |
| `musiton-plugin-host` | Placeholder for VST3 hosting (phase 12). The only crate that will be allowed to use `unsafe`. |
| `musiton-app` | The program: menus and dialogs, settings, autosave and crash recovery, export, demo song, installer configuration. |

Tests live in each crate's `tests/` folder (integration and golden-audio tests) and in `#[cfg(test)]` modules next to the code. Benchmarks are in `musiton-devices/benches/`.
