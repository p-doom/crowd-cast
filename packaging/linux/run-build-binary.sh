#!/usr/bin/env bash
# Host driver: build the crowd-cast-agent binary on the glibc-2.34 floor, in the SAME AlmaLinux 9
# image as the libobs bundle (run-build.sh). Mirrors run-build.sh: builds the (cached) image, then
# runs build-binary.sh with a PERSISTENT cargo cache (registry + target survive across runs) and the
# repo source mounted READ-ONLY. Needs the bundle tarball in out/ — run run-build.sh first.
# Output -> packaging/linux/out/crowd-cast-agent-x86_64.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
ENGINE="${ENGINE:-$(command -v podman || command -v docker)}"
IMG=crowd-cast-obs-builder:el8
CARGO_CACHE="$HERE/.cache-rust"; OUT="$HERE/out"; mkdir -p "$CARGO_CACHE" "$OUT"

"$ENGINE" build -t "$IMG" "$HERE"

# Repo mounted :ro (no :z relabel — it would recursively relabel a multi-GB target/; this box has no
# enforcing SELinux). The small mounts use :z to match run-build.sh.
MOUNTS=(
  -v "$REPO:/src:ro"
  -v "$HERE/build-binary.sh:/build-binary.sh:ro,z"
  -v "$CARGO_CACHE:/cargo:z"
  -v "$OUT:/out:z"
)
# Hermetic build config: when the working tree carries the dev box's untracked .cargo/config.toml
# (which forces `-fuse-ld=mold` — unusable with the floor image's GCC 11), overlay an EMPTY config so
# the build matches a clean CI git checkout. A clean checkout has no such file, so skip the overlay
# there (mounting over a non-existent path inside the :ro /src mount would fail).
if [ -f "$REPO/.cargo/config.toml" ]; then
  EMPTY_CARGO_CFG="$CARGO_CACHE/empty-config.toml"; : > "$EMPTY_CARGO_CFG"
  MOUNTS+=( -v "$EMPTY_CARGO_CFG:/src/.cargo/config.toml:ro" )
fi

"$ENGINE" run --rm \
  "${MOUNTS[@]}" \
  -e CARGO_HOME=/cargo/home \
  -e CARGO_TARGET_DIR=/cargo/target \
  -e CROWD_CAST_OBS_ABI="${CROWD_CAST_OBS_ABI:-32.0.2}" \
  -e CROWD_CAST_BUILD_NUMBER="${CROWD_CAST_BUILD_NUMBER:-0}" \
  -e CROWD_CAST_API_GATEWAY_URL="${CROWD_CAST_API_GATEWAY_URL:-https://placeholder.invalid/prod/presign}" \
  -e CROWD_CAST_UPDATE_FEED_URL="${CROWD_CAST_UPDATE_FEED_URL:-}" \
  -e CROWD_CAST_UPDATE_PUBKEY="${CROWD_CAST_UPDATE_PUBKEY:-}" \
  -e CROWD_CAST_GOOGLE_CLIENT_ID="${CROWD_CAST_GOOGLE_CLIENT_ID:-}" \
  -e CROWD_CAST_GOOGLE_CLIENT_SECRET="${CROWD_CAST_GOOGLE_CLIENT_SECRET:-}" \
  "$IMG" /build-binary.sh
echo "=== out ==="; ls -lh "$OUT/crowd-cast-agent-x86_64"
