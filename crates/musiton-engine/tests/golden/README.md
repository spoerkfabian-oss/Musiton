# Golden renders: engine

Reference audio that the engine tests compare against, sample by sample (tolerance 1e-5).

| File | Rendered by | Content |
|---|---|---|
| `metronome_4bars_120bpm.wav` | `tests/transport.rs` → `golden_four_bars_at_120_bpm` | 4 bars of the metronome at 120 BPM, 48 kHz, mono 32-bit float (accent on beat 1). |

When the engine's sound changes on purpose, regenerate it and listen to the result before committing:

```bash
MUSITON_BLESS=1 cargo test -p musiton-engine --test transport
```
