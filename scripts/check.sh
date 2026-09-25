#!/usr/bin/env bash
# Everything CI checks, locally. Exits non-zero on the first failure.
# Set OT_TEST_REDIS_URL and OT_TEST_NATS_URL to include the Redis- and
# NATS-backed tests (a JetStream server: `docker run -p 4222:4222 nats:2 -js`).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all --check
cargo clippy --quiet --all-targets --locked -- -D warnings
cargo test --quiet --locked
(cd ui && npm run --silent lint && npm run --silent build >/dev/null)
echo "all checks passed"
