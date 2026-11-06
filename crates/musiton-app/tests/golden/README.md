# Golden renders: app (whole project)

Reference audio for rendering a complete project through the app's graph builder.

| File | Rendered by | Content |
|---|---|---|
| `filter_sweep.wav` | `src/engine_graph.rs` → `automated_filter_sweep_renders_identically` | A polysynth note (A2) whose filter cutoff follows a curved automation lane: the first 2 s (4 beats at 120 BPM), left channel, 48 kHz, mono 32-bit float. |

Two tests use it:
- `automated_filter_sweep_renders_identically` checks that the render matches this file within 1e-4 and is identical with 256-, 100- and 333-sample device buffers.
- `export::tests::float_export_is_bit_identical_to_the_render` checks that a 32-bit float export equals a direct render and matches this file within 1e-4.

The tolerance allows for tiny `sin`/`exp` differences between Windows and Linux.

To regenerate after an intended change (listen before committing):

```bash
MUSITON_BLESS=1 cargo test -p musiton-app automated_filter_sweep
```
