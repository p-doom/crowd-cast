#!/usr/bin/env bash
# Host driver: build the (cached) image, then run the bundle build with a PERSISTENT cache
# (so x264/FFmpeg/OBS resume across runs) and the build script MOUNTED (edit + re-run
# without rebuilding the image). Output -> packaging/linux/out/.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE="${ENGINE:-$(command -v podman || command -v docker)}"
IMG=crowd-cast-obs-builder:el8
CACHE="$HERE/.cache"; OUT="$HERE/out"; mkdir -p "$CACHE" "$OUT"

"$ENGINE" build -t "$IMG" "$HERE"
"$ENGINE" run --rm \
  -v "$CACHE:/build:z" \
  -v "$HERE/build-bundle.sh:/build-bundle.sh:ro,z" \
  -v "$HERE/patches:/patches:ro,z" \
  -v "$OUT:/out:z" \
  -e OBS_TAG="${OBS_TAG:-32.0.2}" -e JOBS="${JOBS:-$(nproc)}" \
  "$IMG" /build-bundle.sh
echo "=== out ==="; ls -lh "$OUT"
