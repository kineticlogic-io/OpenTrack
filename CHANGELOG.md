# Changelog

## Unreleased

### Tracker profiles

- **What a profile is.** A sensor's tracker settings as a JSON file, with:
  - its sensor kind and platform
  - where the numbers come from
  - the tracker stage it sets
- **Where they live.**
  - `profiles/trackers/` ships with OpenTrack (`OT_PROFILES_DIR`).
  - Imported profiles go to `profiles/trackers` beside the database.
- **In a source's tracker stage** you can load a profile, save the tuned stage as one, import a
  file, or download one. The stage records which profile it came from and shows when it has
  changed since.
- **API.** `GET/POST /tracker-profiles`, `GET/DELETE /tracker-profiles/{name}`. Imports and deletes
  go in the decision log.
- **Benchmark.** `bench run --profile SOURCE=NAME`.
- **The shipped profiles** span the sensor kinds with a tracker stage:
  - GMTI:
    - `gmti-wide-area`: clutter density 2e-7, a balance between noise and real traffic
    - `gmti-high-clutter`: 1e-6, for very noisy data such as the Garden Island recordings
    - `globalhawk-mti`, `lynx-gmti`, `astor-dmti`: nominal starting points
  - Maritime MTI: `lsrs-maritime-mti`
  - Radars: `marine-radar-x`, `coastal-surveillance-radar`, `air-surveillance-radar`
  - Lidar: `lidar-surface`
- **The STANAG 4607 example** uses `gmti-wide-area`. Its old clutter density, 3e-8, made about 1,150
  tracks in 40 minutes on the Garden Island recordings.

### correlation-4

Radar tracks now join their AIS and ADS-B tracks. On the benchmark:

| Scenario | Recall | Precision | GOSPA |
|---|---|---|---|
| AIS + coastal radar | 49% → 78% | 98% → 96% | 11,869 → 7,151 |
| ADS-B + radar | 50% → 90% | 100% | 17,866 → 6,862 |

The changes:

- A slow feed's last report is reused against a sensor's newer ones, weighted and at most every
  10 s.
- Stopped targets are propagated as a slow drift.
- Aircraft get their own velocity spread.
- Views up to 180 s old are compared.
- The same-source window has its own setting.

The new settings are in the correlation form. See [docs/algorithms.md](docs/algorithms.md).

## 0.2.0 (alpha), 2026-09-26

### Plugins

You can now add codecs, trackers and pairing scorers of your own. The full guide is
[docs/plugins.md](docs/plugins.md).

- **Interface.** The plugin interface is `wit/plugin.wit` (`opentrack:plugin@0.1.0`) and covers
  three kinds of plugin:
  - **codec**: turns frames into records.
  - **tracker**: turns plots into tracks, as a source's tracker stage (`"algorithm": "plugin"`).
  - **scorer**: scores whether a report and a system track are the same object. The engine keeps
    its own pairing test, thresholds and decisions. Each merge decision records the plugin, its
    version and what it said.
- **Two ways to run a plugin**, and the engine treats them the same:
  - **WebAssembly components** run in OpenTrack's process, sandboxed. You set each plugin's grants:
    memory, time per call, directories, network addresses and environment. Every stream gets its
    own instance of the plugin.
  - **External programs** serve the same interface over a socket. Use these for Python with numpy
    or Stone Soup, or for a GPU.
- **Settings → Plugins** lets you:
  - add a component or an external plugin's address
  - enable, disable, grant, check and delete plugins
  - see where each plugin is used

  Every role picks up changes within 5 s, and every change goes in the decision log. The tracker
  stage and the correlation settings choose plugins with forms built from each plugin's manifest.
- **Command line.** `opentrack plugin check|add|list`.
- **SDKs** (`sdk/`):
  - **Rust** builds components. Examples: an alpha-beta tracker, a CSV codec and a speed-veto
    scorer.
  - **Python** serves plugins externally, or builds plain Python into components with
    componentize-py. Examples: a domain and course scorer, and a Stone Soup GNN tracker.
- **Built-in plugins.** STANAG 4607 is now a built-in plugin in the same registry.

### Benchmark

- **`opentrack bench`** runs a recorded scenario through the real pipelines and engine, on the
  scenario's own clock and as fast as they go, and publishes nothing.
- **`scripts/benchmark/`** builds and scores 27 scenarios, compares runs, and sweeps settings
  (`--set`) and plugins (`--plugin`). The scenarios are:
  - nine Autoferry lidar and radar recordings, each with and without a vessel feed
  - Garden Island GMTI with the exercise GPS
  - Stone Soup's Solent AIS and OpenSky ADS-B with simulated radars
  - a synthetic case of targets crossing in heavy clutter
- **Scores** cover:
  - GOSPA
  - SIAP-style completeness, false tracks, accuracy and identity
  - track counts
  - correlation precision and recall
- **Data stays out of the repository**, in `$OT_BENCH_DATA`.
- The replay tools moved to `scripts/benchmark/replay/`.

### Engine

- **Scenario clock.** Lifecycle and timers follow the scenario's clock in a replay.
- **Non-blocking reads.** A zero read block now returns at once. Redis checks BLOCK timeouts only
  about every 100 ms, which made fast replays 30 times slower.

### Upgrading

- **Migration 0012** adds the `plugins` table. It replaces an unused placeholder, and runs on
  start.
- **Container builds** now copy `wit/`.

## 0.1.0 (alpha), 2026-09-26

The first alpha has:

- **Sources**:
  - transports: TCP, UDP, HTTP polling, WebSocket, MQTT and file
  - codecs: JSON, XML, Cursor-on-Target and STANAG 4607
  - a pipeline designer with live preview
- **Trackers**: GNN and MHT, each track with an existence probability.
- **Correlation** (correlation-3): covariance-propagated likelihood-ratio pairing, splits, and a
  decision log and track graph.
- **Entities**: one Entity model, with pipeline links in either direction and a registry
  spreadsheet.
- **Track management**: pair, merge, group (battle groups, flights, convoys with 2525C echelon
  symbols), delete and designate.
- **Output**: NATS JetStream in an OTH-GOLD style message.
- **UI**: React on stareSDK.
