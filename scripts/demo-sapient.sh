#!/usr/bin/env bash
# Wires the SAPIENT demo: ensures OpenTrack is up, seeds the SAPIENT-MQTT source,
# and tails source status. Run after `docker compose -f docker-compose.sapient.yml up -d`.
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
OT_HOST="${OT_HOST:-127.0.0.1:8090}"
OT_BIN="${OT_BIN:-$ROOT_DIR/target/debug/opentrack}"
SIM_ROOT="${SIM_ROOT:-$ROOT_DIR/../SAPIENT/rust-sapient}"
SIM_BIN="${SIM_BIN:-$SIM_ROOT/target/debug/sapient-sim}"
SIM_SCENARIO="${SIM_SCENARIO:-$SIM_ROOT/scenarios/multi-asset-london.yml}"
DATA_DIR="${DATA_DIR:-$ROOT_DIR/data-sapient}"
SOURCE_SPEC="$ROOT_DIR/docs/examples/sapient-mqtt.json"

# 1. Ensure OpenTrack binary and data dir exist.
if [ ! -x "$OT_BIN" ]; then
  echo "ERROR: opentrack binary not found at $OT_BIN" >&2
  exit 1
fi
mkdir -p "$DATA_DIR"

# 2. Start OpenTrack (all roles) in the background if not already running.
if ! pgrep -f "opentrack all" >/dev/null 2>&1; then
  echo "Starting OpenTrack..."
  OT_BIND="$OT_HOST" \
    OT_REDIS_URL="redis://127.0.0.1:6379" \
    OT_NATS_URL="nats://127.0.0.1:4222" \
    OT_AUTH=off \
    OT_SQLITE_PATH="$DATA_DIR/opentrack.db" \
    OT_LOG=info \
    "$OT_BIN" all &
  OT_PID=$!
  echo "OpenTrack PID: $OT_PID"
else
  echo "OpenTrack already running."
  OT_PID=""
fi

# 3. Wait for the API to become reachable.
echo "Waiting for OpenTrack API at $OT_HOST..."
for i in $(seq 1 30); do
  if curl -sf "http://$OT_HOST/healthz" >/dev/null 2>&1; then
    echo "  OK ($i tries)"
    break
  fi
  sleep 1
done
if ! curl -sf "http://$OT_HOST/healthz" >/dev/null 2>&1; then
  echo "ERROR: OpenTrack did not become healthy at $OT_HOST" >&2
  exit 1
fi

# 4. Create or update and enable the SAPIENT-MQTT source.
echo "Configuring SAPIENT-MQTT source..."
curl -sf -X PUT "http://$OT_HOST/api/v1/sources/sapient-mqtt" \
  -H 'Content-Type: application/json' \
  -d @"$SOURCE_SPEC" \
  | jq '{id, revision, enabled}'
curl -sf -X POST "http://$OT_HOST/api/v1/sources/sapient-mqtt/enable" \
  | jq '{id, revision, enabled}'

# 5. Start the SAPIENT simulator (if binary exists).
if [ -x "$SIM_BIN" ] && [ -r "$SIM_SCENARIO" ]; then
  echo "Starting SAPIENT simulator..."
  SAPIENT_BROKER="mqtt://127.0.0.1:1883" \
    SAPIENT_INTERVALS_DETECTION=2s \
    SAPIENT_INTERVALS_STATUS=5s \
    SAPIENT_LOG=info \
    "$SIM_BIN" --scenario "$SIM_SCENARIO" &
  SIM_PID=$!
  echo "Simulator PID: $SIM_PID"
else
  echo "WARNING: simulator binary or scenario not found; skipping." >&2
  echo "         Binary:   $SIM_BIN" >&2
  echo "         Scenario: $SIM_SCENARIO" >&2
fi

# 6. Tail source status.
echo ""
echo "=== Watching source status (Ctrl-C to stop) ==="
while true; do
  curl -s "http://$OT_HOST/api/v1/sources/sapient-mqtt" \
    | jq -r '[.id, (.enabled|tostring), (.status.link.connected|tostring), ((.status.totals_since_start.plots // 0)|tostring)] | @tsv' \
    2>/dev/null
  echo "--- $(date +%T) ---"
  sleep 5
done
