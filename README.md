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
| 3. Correlation engine | Source vs system tracks, pairing approaches, best source | next |
| 4. Track management | Pair, unpair, merge, delete, groups, decision log with undo | |
| 5. Codecs and plugin SDK | Protobuf from `.proto`, brokers, WebAssembly plugins | |
| 6. Migration and cutover | aisstream / adsb.lol examples, parallel run | |

## Layout

```
crates/
  ot-core     authoritative schema (Observation), system tracks, GOLD UIDs, OTH-GOLD minimum and the published message (opentrack.track.v2)
  ot-store    SQLite (decisions, config, temporal track graph) and Redis (streams, live state)
  ot-nats     NATS JetStream publisher (stream setup, acknowledged publishes, status)
  ot-source   source framework: transports, framing, codecs, mapping, registry grading, filter, throttle
  ot-server   the `opentrack` binary: serve | sources | engine | writer | all | migrate | synthetic | retire
docs/examples aisstream and adsb.lol as pure configuration (see docs/examples/README.md)
docs/nats-output.md  the published track contract, for consumers
ui/           React + TypeScript (Vite) on openstare's stareSDK components: Overview (status and
              system metrics), Sources (topology, list, add-source wizard, mapping studio with live
              preview), Track Database (map, baseball card with provenance, card editor, tracks
              table), Schema workspace
```

Sources: a transport (`tcp_client`, `tcp_server`, `udp` with multicast, `http_poll`, `websocket`,
`mqtt`), a codec and a mapping. Transport metadata reaches the mapping under `_frame`: an MQTT
message's topic is `_frame.topic`, and `_frame.topic_levels[1]` is its second level, so an id
carried in the topic (`ais/366123456/pos`) can be the track key or an identifier. Secrets are
written as `${env:NAME}` and resolved when the source starts.

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
