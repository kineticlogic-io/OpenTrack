# OpenTrack

A source-agnostic track management server. OpenTrack onboards any number of track feeds, maps
them into one authoritative track schema, correlates them into system tracks (OTH-GOLD style), gives
operators pair, merge, split and delete tools, and publishes the result to NATS JetStream.

Design and roadmap: [Track Management Server — Design & Roadmap](https://claude.ai/code/artifact/3876ba1a-0e0f-49fd-9f70-47e1c2bb8e7d).

## Status

| Phase | Scope | State |
|-------|-------|-------|
| 0. Foundations | Workspace, SQLite + track graph, Redis layout, core schema, NATS writer, UI shell | **done** |
| 1. Source framework | Transports (TCP, UDP, HTTP poll, WebSocket, MQTT), JSON / CoT / XML codecs, mapping, enrich, filter, throttle, workers, 1:1 engine | **done** |
| 2. Onboarding UI and schema | Add-source wizard, probe, mapping studio, schema workspace | **done** |
| 3. Correlation engine | Source vs system tracks, pairing approaches, best source | **done**: identifier and kinematic pairing with error propagation and pairing probabilities, suggest mode, splits, do-not-pair, runtime settings, detection association, GNN/MHT tracker stage with existence probabilities and auto timing; vector similarity deferred |
| 4. Track management | Pair, unpair, merge, delete, groups, decision log with undo | |
| 5. Codecs and plugin SDK | Protobuf from `.proto`, brokers, WebAssembly plugins | **started**: codec plugin interface (compiled in), STANAG 4607 GMTI plugin, length-field framing, file transport |
| 6. Migration and cutover | aisstream / adsb.lol examples, parallel run | |

## Layout

```
crates/
  ot-core     authoritative schema (Observation), system tracks, GOLD UIDs, OTH-GOLD minimum and the published message (opentrack.track.v2)
  ot-store    SQLite (decisions, config, temporal track graph) and Redis (streams, live state)
  ot-nats     NATS JetStream publisher (stream setup, acknowledged publishes, status)
  ot-source   source framework: transports, framing, codecs and codec plugins, mapping, registry grading, filter, tracker (GNN/MHT), throttle
  ot-codec-stanag4607  STANAG 4607 (Edition 3) GMTI decoder: every segment type, typed and as JSON records
  ot-server   the `opentrack` binary: serve | sources | engine | writer | all | migrate | synthetic | retire
docs/examples aisstream, adsb.lol, STANAG 4607 GMTI, a GPS feed and the Autoferry demo as pure configuration
scripts/     replay tools: gmti-rebroadcast.py (4607 recordings over TCP), gmti-gps-replay.py (GMTI and
             GPS logs of one exercise on one clock), autoferry-replay.py (the Autoferry demo)
docs/nats-output.md  the published track contract, for consumers
ui/           React + TypeScript (Vite) on openstare's stareSDK components: Overview (status and
              system metrics), Sources (topology, list, add-source wizard, mapping studio with live
              preview), Correlation (suggestions, settings, decisions), Track Database (map,
              baseball card with provenance, card editor, tracks table), Registry (entities,
              identifiers and cards; spreadsheet export and import), Schema workspace, Settings
              (site name, classification banner, data export, purge)
```

Sources: a transport (`tcp_client`, `tcp_server`, `udp` with multicast, `http_poll`, `websocket`,
`mqtt`), a codec and a mapping. Transport metadata reaches the mapping under `_frame`: an MQTT
message's topic is `_frame.topic`, and `_frame.topic_levels[1]` is its second level, so an id
carried in the topic (`ais/366123456/pos`) can be the track key or an identifier. Secrets are
written as `${env:NAME}` and resolved when the source starts.

A source reports either **tracks** (a key per object: AIS, ADS-B, TAK, a radar's own tracks) or
**detections** (`"reports": "detections"`: anonymous plots). Detections either update the nearest
system track another source keeps, or, with a **tracker** stage in the pipeline, become tracks
first: `"tracker": {"algorithm": "gnn"}` (global nearest neighbour) or `"mht"` (multiple hypothesis
tracking, fewer false tracks in clutter). A tracker's tracks carry no identity: their
classification is unknown affiliation and at most a domain (ground, air, surface, subsurface), from
the stage's `domain` or the plots' own. Correlation then pairs them with other sources' tracks on
kinematic agreement.

Every tracker track carries the probability that it is a real target (its **existence**, from how
well its plots fit against clutter and how often it goes unseen), which confirms and drops it and
is published as its `confidence`; its reports carry the position error ellipse and the full
position and velocity covariance. Correlation compares tracks with that covariance propagated in
time (position, and velocity when both report it), keeps a probability that two source tracks are
the same object, pairs at 0.99 and follows it afterwards as the pairing's confidence. A system
track's confidence combines its sources' existence and pairing confidences, and is an output
schema built-in. Details and scores: [docs/algorithms.md](docs/algorithms.md).

**Codec plugins** decode formats beyond JSON and XML: `"codec": {"type": "plugin", "plugin":
"stanag4607", "options": {...}}`, with the options and framing each plugin describes at
`GET /api/v1/plugins` (the Sources form shows them). Plugins are compiled in; the interface is the
one a sandboxed (WebAssembly) plugin will implement. **STANAG 4607** (NATO GMTI) streams over TCP,
framed by the packet header's size field; [docs/examples/stanag4607.json](docs/examples/stanag4607.json)
turns every dwell target into a detection with a ground error ellipse from the radar geometry, forms
ground tracks with the GNN tracker, times the tracker by the revisit rate it measures in the stream
(`auto_timing`), and publishes the sensor platform itself as a friendly track (`publish_platform`,
symbol from the mission's platform type). A mapping rule of kind `track` bypasses the tracker.

An **output filter** (Correlation settings) decides what is published: include and exclude
bounding boxes, allowed affiliations, domains and track types, a minimum confidence, and an
optional rule over the track's fields. A track that fails stays inside OpenTrack, marked
filtered with the reason; a published track that stops passing (it leaves the area, say) is
deleted downstream until it passes again. Per source, the pipeline's filter stage drops reports
on the same kinds of rules before they reach correlation.

Only authoritative system tracks are published: confirmed, and reported for by a source that may
stand alone (`publish_alone`; by default track feeds may, detection feeds may not). A track only
sensors report for, even several agreeing with each other, stays inside OpenTrack (the track card
marks it "not published") until a track feed reports for it too. A source ends a track with
`state: dropped`; a system track no source reports for any more is retired at once.

The **Correlation** workspace shows the engine's suggestions (pairings in suggest mode, and
splits when a source track stops agreeing with its track) for an operator to accept or reject,
the correlation settings (saved and applied while running), and every correlation decision with
its evidence. A track's Provenance tab shows how each source track was paired, and splits one
off. The same operations are API calls: `/api/v1/correlation/settings`,
`/api/v1/correlation/suggestions/{id}/accept|reject`, `/api/v1/tracks/{uid}/split`,
`/api/v1/tracks/merge`, `/api/v1/tracks/do-not-pair`. Algorithm versions and their scores are in
[docs/algorithms.md](docs/algorithms.md).

Storage split: **SQLite** holds everything a person decided or configured (sources, schema,
mappings, entity cards, the audit log, the track graph); **Redis** holds everything feeds produce
(`tms:obs:*` streams, `tms:src:*` / `tms:sys:*` live state, the `tms:out` outbox, metrics).

System tracks are published to NATS JetStream, one subject per track: `tracks.tms-<UID>`, where the
UID is a GOLD-style 3-character site code plus a 9-digit sequence (`tracks.tms-OTK000000042`). The
`TRACKS` stream keeps the latest message per track, so a consumer that starts late still gets the
whole picture. Messages are JSON `opentrack.track.v2` (`upsert` or `delete`); the full contract is
in [docs/nats-output.md](docs/nats-output.md).

What a track message carries:

* **The OTH-GOLD minimum**, always: track number, class-name, force code, track type, time and
  position (the mandatory CTC and POS fields of OS-OTG Rev C), plus a required symbol code
  (`sidc`: MIL-STD-2525C, 2525D or a CoT type, with its standard named).
* **`attributes`**, designed by the admin in the Schema workspace as an output schema (field name,
  type, notes). A field's value comes from the entity's **card** (the baseball card an admin fills
  in on the Cards page, e.g. a ship's contact phone), else from a **feed** mapped to it at
  onboarding, or from an OpenTrack **built-in** it is linked to (state, speed, identifiers...).
  The card is the authority; where a feed disagrees, the track and the card show a warning.

A card belongs to a registry entity, which tracks reach through their identifiers of any scheme;
**Create card** on a looked-up track starts one with that track's identifiers.

The **Registry** tab lists and searches every entity with its identifiers, registry fields and
card, and opens its card. The whole registry exports as a spreadsheet (XLSX or CSV,
`GET /api/v1/registry/export?format=xlsx`), one row per entity with `entity_id`, `name`,
`status`, `id:<scheme>`, `registry:<key>` and `card:<field>` columns, and imports the same way
(`POST /api/v1/registry/import-sheet`): a dry run shows each row's change first, a row updates
the entity its id or identifiers name or creates one, blank cells change nothing, an identifier is
never taken from another entity, and nothing is written while any row has an error.

The **Settings** tab sets the instance's display name (track UIDs keep the deployment's site code)
and the **classification banner**, drawn top and bottom of every page as OpenStare draws it: off,
set here (with the standard markings as presets), or following OpenStare's own banner
(`<OpenStare>/api/public/banner`, read every minute; the marking set here stays up if OpenStare
cannot be read). OpenTrack serves its banner the same way, at `GET /api/v1/public/banner`. It also
exports the live tracks (GeoJSON or CSV, as published, with state, confidence and sources) and
the configuration (sources, schema versions, correlation and instance settings, as one JSON file),
and **purges** the tracks: every live track is retired (published ones are deleted downstream),
optionally with the track graph's history; configuration, registry, cards and the decision log
stay. A purge asks for the site code to be typed.

## Running

Requires Rust (stable), Node 22, Redis and a NATS server with JetStream enabled.

```sh
cargo build --release
(cd ui && npm ci && npm run build)

# every role in one process: control plane (API + UI on :8090), sources, engine, writer
./target/release/opentrack all

# add a source through the API, then enable it
curl -X POST -H 'content-type: application/json' --data @docs/examples/adsb-lol.json localhost:8090/api/v1/sources
curl -X POST localhost:8090/api/v1/sources/adsb-lol/enable

# end-to-end check: push a synthetic track through, verify it in the stream, then retire it
./target/release/opentrack synthetic --verify --retire
```

Set `OT_NATS_STREAM=OPENTRACK_SELFTEST OT_NATS_TRACKS_SUBJECT=opentrack.selftest` (and a separate
`OT_REDIS_NAMESPACE`) on both `writer` and `synthetic` to exercise the pipeline without touching the
operational `tracks.>` subjects.

UI development: `opentrack serve` plus `cd ui && npm run dev` (Vite proxies `/api` to :8090).

Or with Docker: `docker compose up --build` (host networking, beside an existing Redis, publishing
to OpenStare's NATS). For a local NATS with JetStream: `docker compose --profile dev-nats up nats`.

### Configuration

| Variable | Default | |
|----------|---------|-|
| `OT_BIND` | `0.0.0.0:8090` | control plane address |
| `OT_SQLITE_PATH` | `data/opentrack.db` | created and migrated on start |
| `OT_REDIS_URL` | `redis://127.0.0.1:6379` | |
| `OT_REDIS_NAMESPACE` | `tms` | prefix for every Redis key |
| `OT_SITE_CODE` | `OTK` | GOLD site code for UIDs |
| `OT_NATS_URL` | `nats://127.0.0.1:4222` | NATS server(s), comma separated |
| `OT_NATS_CREDS` / `OT_NATS_TOKEN` / `OT_NATS_USER` + `OT_NATS_PASSWORD` | | NATS auth, if the server requires it |
| `OT_NATS_STREAM` | `TRACKS` | JetStream stream (created if missing, never modified) |
| `OT_NATS_TRACKS_SUBJECT` | `tracks` | subject prefix: tracks go to `<prefix>.tms-<UID>` |
| `OT_NATS_MAX_AGE_HOURS` | `24` | message age limit for a stream OpenTrack creates |
| `OT_WRITE_MIN_INTERVAL_SECS` | `5` | per-track write coalescing |
| `OT_UI_DIR` | `ui/dist` | built UI served at `/` |
| `OT_LOG`, `OT_LOG_FORMAT` | `info`, text | `OT_LOG_FORMAT=json` for JSON logs |

## Tests

```sh
cargo test                                              # unit tests
OT_TEST_REDIS_URL=redis://127.0.0.1:6379 \
OT_TEST_NATS_URL=nats://127.0.0.1:4222 \
OT_TEST_MQTT_URL=mqtt://127.0.0.1:1883 cargo test      # plus the Redis, NATS and MQTT tests
cd ui && npm run lint && npm run build
```

The Redis tests run in their own key namespace, the NATS tests in their own stream and the MQTT
tests on their own topics, and all clean up afterwards. The NATS tests need JetStream
(`docker run -p 4222:4222 nats:2 -js`); for MQTT any broker works
(`docker run -p 1883:1883 eclipse-mosquitto:2 mosquitto -c /mosquitto-no-auth.conf`).

## UI components

The UI uses only components from openstare's **stareSDK** (Elite Command Design System), vendored
as a packed build in `ui/vendor/` because the package is private and unpublished. Refresh it
from a local openstare checkout with `scripts/vendor-staresdk.sh`. Page layout uses stareSDK
tokens (`var(--…)`) only; new reusable components go upstream into stareSDK, not into this repo.
