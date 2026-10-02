#!/usr/bin/env bash
# Rebuild ui/vendor/staresdk-<version>.tgz from a local openstare checkout.
#
# stareSDK is a private, unpublished workspace package inside the openstare
# repo, so OpenTrack vendors a packed build rather than fetching it. This keeps
# OpenTrack's own build and CI free of access to the openstare repository.
#
# Usage: scripts/vendor-staresdk.sh [path-to-openstare]   (default ~/src/openstare)
set -euo pipefail

src="${1:-$HOME/src/openstare}"
root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cp -r "$src/stareSDK/." "$work/"
rm -rf "$work/node_modules" "$work/dist"
# --legacy-peer-deps: jsdom's optional `canvas` peer trips an npm arborist bug.
(cd "$work" && npm install --no-audit --no-fund --legacy-peer-deps && npm run build && npm pack)

rm -f "$root"/ui/vendor/staresdk-*.tgz
cp "$work"/staresdk-*.tgz "$root/ui/vendor/"
sha="$(git -C "$src" rev-parse --short HEAD)"
tgz="$(basename "$work"/staresdk-*.tgz)"
echo "$tgz built from kineticlogic-io/OpenStare@$sha stareSDK/ on $(date -u +%F)" > "$root/ui/vendor/STARESDK_SOURCE"
echo "vendored $tgz; now run: (cd ui && npm install ./vendor/$tgz)"
