# tools/

Helper programs that belong to Musiton but run as separate processes.

| Folder | What it is | State |
|---|---|---|
| `scanner/` | `musiton-scan`: scans VST3 plugins in its own process, so a plugin that crashes while loading cannot take Musiton down. It writes a JSON plugin cache that the browser reads. | Placeholder (an empty `main`). It is built in phase 12. |
