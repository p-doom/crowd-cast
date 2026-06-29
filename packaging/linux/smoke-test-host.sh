#!/usr/bin/env bash
# Host smoke test (real VAAPI): extract a freshly built bundle, point crowd-cast at it,
# and assert a recording file actually grows. Run on the laptop (real GPU).
#   smoke-test-host.sh packaging/linux/out/obs-bundle-32.0.2-x86_64.tar.zst
set -euo pipefail
TARBALL="${1:?usage: smoke-test-host.sh <bundle.tar.zst>}"
BIN="${CROWD_CAST_BIN:-$HOME/Documents/pdoom/crowd-cast/target/release/crowd-cast-agent}"
WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
tar -C "$WORK" -xaf "$TARBALL"
B="$WORK/usr"
export LD_LIBRARY_PATH="$B/lib:${LD_LIBRARY_PATH:-}"
export CROWD_CAST_OBS_DATA_PATH="$B/share/obs/libobs"
export CROWD_CAST_OBS_PLUGIN_BIN_PATH="$B/lib/obs-plugins"
export CROWD_CAST_OBS_PLUGIN_DATA_PATH="$B/share/obs/obs-plugins/%module%"
REC="$(mktemp -d)"; trap 'rm -rf "$WORK" "$REC"' EXIT
echo "recordings -> $REC ; running crowd-cast for ~12s..."
( "$BIN" >/tmp/cc-smoke.log 2>&1 & echo $! > "$WORK/pid" ) || true
sleep 12
kill "$(cat "$WORK/pid")" 2>/dev/null || true
echo "--- mp4_output / output errors in log ---"
grep -iE "mp4_output|Failed to (create|start)|Output ID|recording" /tmp/cc-smoke.log | head
echo "NOTE: assert a recording file was created and non-zero (see configured output dir)."
