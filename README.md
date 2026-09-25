# OpenTrack

A source-agnostic track management server. OpenTrack onboards any number of track feeds, maps
them into one authoritative track schema, correlates them into system tracks (OTH-GOLD style), gives
operators pair, merge, split and delete tools, and publishes the result to peat-node.

Design and roadmap: [Track Management Server — Design & Roadmap](https://claude.ai/code/artifact/3876ba1a-0e0f-49fd-9f70-47e1c2bb8e7d).

## Status

| Phase | Scope | State |
|-------|-------|-------|
| 0. Foundations | Workspace, SQLite + track graph, Redis layout, core schema, peat writer, UI shell | **done** |
| 1. Source framework | Transports, JSON / CoT codecs, mapping, enrich, filter, throttle, workers, 1:1 engine | **done** |
| 2. Onboarding UI and schema | Add-source wizard, probe, mapping studio, schema workspace | next |
| 3. Correlation engine | Source vs system tracks, pairing approaches, best source | |
| 4. Track management | Pair, unpair, merge, delete, groups, decision log with undo | |
| 5. Codecs and plugin SDK | Protobuf from `.proto`, brokers, WebAssembly plugins | |
| 6. Migration and cutover | aisstream / adsb.lol examples, parallel run | |

## Layout

```
crates/
  ot-core     authoritative schema (Observation), system tracks, GOLD UIDs, peat-node wire format
  ot-store    SQLite (decisions, config, temporal track graph) and Redis (streams, live state)
  ot-peat     peat-node sidecar gRPC client (proto compiled with protox; no protoc needed)
  ot-source   source framework: transports, framing, codecs, mapping, registry grading, filter, throttle
  ot-server   the `opentrack` binary: serve | sources | engine | writer | all | migrate | synthetic | retire
docs/examples aisstream and adsb.lol as pure configuration (see docs/examples/README.md)
proto/        peat_sidecar.proto
ui/           React + TypeScript (Vite) on openstare's stareSDK components
```

Storage split: **SQLite** holds everything a person decided or configured (sources, schema,
mappings, the audit log, the track graph); **Redis** holds everything feeds produce
(`tms:obs:*` streams, `tms:src:*` / `tms:sys:*` live state, the `tms:out` outbox, metrics).

System tracks are published to peat-node's `tracks` collection as `tms-<UID>`, where the UID is a
GOLD-style 3-character site code plus a 9-digit sequence (`tms-OTK000000042`).

## Running

Requires Rust (stable), Node 22, Redis and a reachable peat-node sidecar.

```sh
cargo build --release
(cd ui && npm ci && npm run build)

# every role in one process: control plane (API + UI on :8090), sources, engine, peat writer
./target/release/opentrack all

# add a source through the API, then enable it
curl -X POST -H 'content-type: application/json' --data @docs/examples/adsb-lol.json localhost:8090/api/v1/sources
curl -X POST localhost:8090/api/v1/sources/adsb-lol/enable

# phase 0 exit check: push a synthetic track through, verify it on peat-node, then retire it
./target/release/opentrack synthetic --verify --retire
```

Use `OT_TRACKS_COLLECTION=opentrack_selftest` on both commands to exercise the pipeline without
touching the authoritative `tracks` collection.

UI development: `opentrack serve` plus `cd ui && npm run dev` (Vite proxies `/api` to :8090).

Or with Docker: `docker compose up --build` (host networking, beside an existing Redis and
sidecar).

### Configuration

| Variable | Default | |
|----------|---------|-|
| `OT_BIND` | `0.0.0.0:8090` | control plane address |
| `OT_SQLITE_PATH` | `data/opentrack.db` | created and migrated on start |
| `OT_REDIS_URL` | `redis://127.0.0.1:6379` | |
| `OT_REDIS_NAMESPACE` | `tms` | prefix for every Redis key |
| `OT_PEAT_ADDR` | `http://127.0.0.1:50051` | peat-node sidecar gRPC |
| `OT_SITE_CODE` | `OTK` | GOLD site code for UIDs |
| `OT_TRACKS_COLLECTION` | `tracks` | where system tracks are published |
| `OT_WRITE_MIN_INTERVAL_SECS` | `5` | per-track write coalescing |
| `OT_UI_DIR` | `ui/dist` | built UI served at `/` |
| `OT_LOG`, `OT_LOG_FORMAT` | `info`, text | `OT_LOG_FORMAT=json` for JSON logs |

## Tests

```sh
cargo test                                              # unit tests
OT_TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test     # plus the Redis outbox-to-sink test
cd ui && npm run lint && npm run build
```

The Redis test runs in its own key namespace and deletes it afterwards.

## UI components

The UI uses only components from openstare's **stareSDK** (Elite Command Design System), vendored
as a packed build in `ui/vendor/` because the package is private and unpublished. Refresh it
from a local openstare checkout with `scripts/vendor-staresdk.sh`. Page layout uses stareSDK
tokens (`var(--…)`) only; new reusable components go upstream into stareSDK, not into this repo.
