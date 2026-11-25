# Golden renders: built-in effects and instruments

One reference render per effect, compared sample by sample by `tests/effects.rs` → `golden_renders` (tolerance 1e-4).

- Input: the stereo test signal from `tests/common/mod.rs` (`test_signal`), 0.5 s at 48 kHz. It is a decaying 100 Hz + 3 kHz tone followed by a quiet tail and a single impulse.
- Settings: each effect uses the parameters listed in `golden_renders`.
- Files: `compressor.wav`, `delay.wav`, `eq.wav`, `limiter.wav`, `reverb.wav`, `saturation.wav`, `utility.wav`. All are stereo 32-bit float.
- The phase 18 effects (`chorus.wav`, `flanger.wav`, `phaser.wav`, `autofilter.wav`, `gate.wav`, `transient.wav`) are rendered with default settings by `tests/effects_more.rs` (`MUSITON_BLESS=1 cargo test -p musiton-devices --test effects_more`).

When an effect's sound changes on purpose, regenerate the files and listen to them before committing:

```bash
MUSITON_BLESS=1 cargo test -p musiton-devices --test effects
```

## Instruments

`tests/instruments.rs` → `golden_renders` renders the first two factory presets of every built-in synth (`<type id>-<preset>.wav`, e.g. `fm-e-piano.wav`): 1 s at 48 kHz of a C-major chord (or, for the drum synth, one hit of each drum) released after 0.5 s. Regenerate with:

```bash
MUSITON_BLESS=1 cargo test -p musiton-devices --test instruments golden
```
