# Plugin SDKs

Write OpenTrack codecs, trackers and pairing scorers of your own. See
[docs/plugins.md](../docs/plugins.md) for the interface, grants and the external protocol.

| SDK | Builds | Examples |
|---|---|---|
| [rust/](rust) | WebAssembly components (`cargo build --release --target wasm32-wasip2`) | `alpha-beta` tracker, `csv-reports` codec, `speed-veto` scorer |
| [python/](python) | external plugins (`python mine.py serve`) or, for plain Python, components (`python mine.py build -o mine.wasm`) | `domain-scorer`, `stonesoup-gnn` tracker |

Add a plugin in Settings → Plugins, or check it first with `opentrack plugin check <file or address>`.
