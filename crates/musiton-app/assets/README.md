# crates/musiton-app/assets/

The application icon.

| File | Used for |
|---|---|
| `icon-256.png` | The window icon (embedded in the program with `include_bytes!` in `src/main.rs`) and the installer's icon list. |
| `icon.ico` | The icon of `musiton.exe` (embedded by `build.rs` with `winresource`, Windows only), the installer icon and the icon of `.musiton` files. It holds 16–256 px sizes. |

Both were drawn by a small Python/Pillow script: a blue rounded square with a waveform of green and blue bars. To change the icon, replace both files with the same sizes and file names.
