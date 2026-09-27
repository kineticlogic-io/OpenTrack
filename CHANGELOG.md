# Changelog

## 0.3.1 (alpha), 2026-09-27

Phase 2, "proven at scale", is met: 16,000 live tracks at 1 Hz on one host, and fewer than 2% wrong
pairings on the harbour benchmark.

### Throughput under live load

- **`bench live`** runs the whole server with a Redis and NATS of its own, fed N tracks at 1 Hz, and
  samples it every 10 s: backlogs, memory, CPU, publications a second, and the delay from report to
  publication.
- **16,000 tracks for 10 minutes:**

  | | Before | After |
  |---|---|---|
  | Engine backlog | 3.4 million, growing | at most 200 |
  | Published | 2,000/s | 3,242/s |
  | Delay (p50 / p99) | 142 / 218 s | 1.0 / 2.0 s |

  It uses about one core and 313 MB.
- **Fixed: the engine dropped reports under load.** A timer abandoned the batch in progress. Timers
  now wait until a batch is finished.
- **The writer publishes concurrently:** one Redis read for every track due, 256 publications in
  flight.
- **Deletes and history-point deletions have their own outbox stream.** It is never trimmed, so a
  writer that falls behind no longer loses them.
- **`OT_OBS_WINDOW_SECS`** sets how long observation streams keep reports (default 600 s). That is
  about 4.8 GB of Redis at 16,000 reports a second.

### correlation-5

- **Splits work against slow feeds.** A radar track that followed another vessel could stay on an
  AIS track that reports every 10 s indefinitely. The split check now has its own window,
  `split.window_secs`, and may reuse the other side's last report.
- **Local density:** a close approach counts for less in a crowded harbour
  (`local_density_radius_m`, 500 m).
- **Automatic splits are the default.**
- **The Autoferry lidar uses MHT**, and so does the `lidar-surface` profile.
- **Harbour wrong pairings: 5.36% → 1.51%.** The costs:
  - more radar and AIS tracks stay apart in the Solent (GOSPA 7,151 → 9,805)
  - more identity changes on two Autoferry recordings

  See [docs/algorithms.md](docs/algorithms.md).

### Benchmark

- **IDF1** for system tracks and each tracker.
- **The harbour gate** appears in every summary, with a stricter figure that judges each pairing at
  each moment.

### UI

- **The History tab** has "Show on map", which draws the track's history as a line, and "Zoom to
  track".
- **stareSDK 0.1.8:** MapView lines and camera fitting.

### Upgrading

- No migrations.
- **Correlation settings saved before 0.3.1 keep their `split.automatic`.** Turn it on in the
  correlation form.

## 0.3.0 (alpha), 2026-09-26

The "secure" phase: sign-in and roles, TLS, undo and history. NATS stays the only output.

### Sign-in and roles

- **Every API call needs a signed-in caller**, in one of three roles:
  - `viewer`: sees everything but accounts and secrets
  - `track_manager`: also manages tracks
  - `admin`: also configures OpenTrack
- **One middleware checks the role each path needs.** A new route that changes something is an
  admin's until it is listed. The decision log now records the signed-in account, which a client
  can no longer set.
- **How callers sign in**, modelled on OpenStare's:
  - local accounts (Argon2id passwords) with a signed `ot_session` cookie
  - SAML 2.0 single sign-on, ported from OpenStare
  - OpenStare's own sign-in and API tokens, when OpenTrack runs beside OpenStare
  - API tokens for machines
  - client certificates over TLS
- **UI:**
  - the sign-in page, like OpenStare's
  - the user menu (change password, sign out)
  - Settings → Users (accounts and API tokens)
  - Settings → Security (sessions, SAML, OpenStare sign-in, client certificates)
  - controls a role cannot use are disabled
- **Warning banner:** as OpenStare's, text users accept after signing in. Declining signs them out.
- **`opentrack user`** manages accounts on a node without the UI.
- **The first admin** comes from `OT_ADMIN_EMAIL` and `OT_ADMIN_PASSWORD`. Without them, it is
  `admin@opentrack.local`, with the password in `initial-admin.txt` beside the database.
- **`OT_AUTH=off`** turns sign-in off for development.

### TLS

- **The API and UI over TLS** with `OT_TLS_CERT` and `OT_TLS_KEY`. With `OT_TLS_CLIENT_CA`,
  client certificates sign machines in.
- **Feeds over TLS and mutual TLS:**
  - a `tls` object on the `tcp_client`, `http_poll`, `websocket` and `mqtt` transports: CA, client
    certificate and key, server name
  - a `tls` object on `tcp_server`: certificate, key, and a client CA that makes client
    certificates required

### Undo

- **What undo covers:** a track manager's pair, unpair, delete, merge, split, "do not pair" and
  group changes, from the decision log (`POST /decisions/{id}/undo`).
- **An undo is a decision of its own.**
- **How it works:** it reverses the decision's links in the temporal track graph.
  - A deleted or merged-away track comes back under its UID.
  - A track the engine made meanwhile for the same source retires.
- **When it is refused:** while a later change by someone else to the same tracks stands.

### Position history

- **What is kept:** each system track's published positions, for `history_hours` (default 12 h).
  At most one point is kept every `history_interval_secs` (default 10 s), both in Settings.
  - A point costs about 130 bytes: 2,000 tracks for 12 h at 10 s is about 1.1 GB of Redis.
- **Reading it:** `GET /tracks/{uid}/history`.
- **Delete history point** (`POST /history/{uid}/delete`): a track manager deletes a bad point.
  - It goes out on NATS as `delete_history_point`, on `tracks.history.tms-<UID>.<ms>`.
  - When the point was the track's latest, the track steps back to the point before.
  - **Consumers of `tracks.>` must skip message kinds they do not handle**
    ([docs/nats-output.md](docs/nats-output.md)).

### Engine

- **One SQLite connection** (0.2.2) and batched writes keep the engine at about 25,000 to 29,000
  observations a second from 2,000 to 16,000 tracks, with history recorded.

### Upgrading

- **Migration 0013** adds accounts, tokens and sign-in settings. It runs on start.
- **Sign-in is on after the upgrade.** Set `OT_ADMIN_EMAIL` and `OT_ADMIN_PASSWORD`, or read
  `initial-admin.txt` beside the database, then sign in.
  - Scripts calling the API need an API token.
- **The image** now needs libxmlsec1, which it installs. `--no-default-features` builds without
  SAML.

## 0.2.2 (alpha), 2026-09-26

### Engine throughput

The engine now handles about 28,000 observations a second, from 2,000 to 16,000 tracks updating
once a second. It handled about 3,900 before.

| Tracks | Before | Engine | Whole run |
|---|---|---|---|
| 2,000 | 3,880/s | 29,400/s | 19,000/s |
| 8,000 | 3,920/s | 27,700/s | 18,100/s |
| 16,000 | not measured | 26,800/s | 17,700/s |

"Whole run" also counts the benchmark decoding and feeding its own data, which the sources
process does in a deployment.

- **One database connection.** The engine keeps one SQLite connection open. It used to open one
  for every decision, and closing it rewrote and deleted the database journal: about 6.5 ms per
  new track, a cap of some 150 new tracks a second.
- **Batched Redis writes.** Within a batch of observations, the engine writes source reports and
  system tracks together in one atomic pipeline, each track once. It used to make about three
  round trips per observation. Publish decisions are still made as each observation arrives.
- **Load scenarios.** `load-2k`, `load-8k` and `load-16k` in `scripts/benchmark/` feed tracks at
  1 Hz for a minute and report observations a second.

### Registry spreadsheet

- **A publish column** (always, never, or empty for automatic) comes after status, so a track
  manager's publish override survives an export and an import.
- **A sheet without the column** leaves every override as it is.

### Documentation

- **Headless running.** The README now describes running OpenTrack headless.

## 0.2.1 (alpha), 2026-09-26

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
