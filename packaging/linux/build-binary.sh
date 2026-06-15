#!/usr/bin/env bash
# Build the crowd-cast-agent binary INSIDE the AlmaLinux 9 container — the SAME glibc-2.34 floor as
# the libobs bundle — so the shipped ELF runs cross-distro. A host build on a bleeding-edge distro
# (e.g. this Manjaro box) would stamp newer GLIBC_2.4x symbols and fail on older targets.
#
# Links against the bundle's libobs (LIBOBS_PATH); the produced binary resolves libobs at RUNTIME
# via its baked $ORIGIN RUNPATH (../share/crowd-cast/obs/<abi>/usr/lib). Output -> /out.
#
# Driven by packaging/linux/run-build-binary.sh — do NOT run on the host (that defeats the floor).
set -euo pipefail

ABI="${CROWD_CAST_OBS_ABI:-32.0.2}"
BUNDLE_TZST="/out/obs-bundle-${ABI}-x86_64.tar.zst"
[ -f "$BUNDLE_TZST" ] || { echo "error: missing bundle $BUNDLE_TZST — run packaging/linux/run-build.sh first" >&2; exit 1; }

# Extract the bundle only to expose libobs.so for LINK-time resolution of -lobs.
BDIR=/tmp/obs-bundle; rm -rf "$BDIR"; mkdir -p "$BDIR"
tar --zstd -xf "$BUNDLE_TZST" -C "$BDIR"
export LIBOBS_PATH="$BDIR/usr/lib"
[ -e "$LIBOBS_PATH/libobs.so" ] || { echo "error: no libobs.so under $LIBOBS_PATH" >&2; exit 1; }

# Build-time config. Dev placeholders unless the caller overrides — a LOCAL floor build is for
# portability validation, not a signed release (the GH workflow passes the real secrets + feed URL).
export CROWD_CAST_API_GATEWAY_URL="${CROWD_CAST_API_GATEWAY_URL:-https://placeholder.invalid/prod/presign}"
export CROWD_CAST_OBS_ABI="$ABI"
# CROWD_CAST_BUILD_NUMBER / _UPDATE_FEED_URL / _UPDATE_PUBKEY / _GOOGLE_CLIENT_ID / _SECRET pass
# through from the environment when set (the workflow sets them; local runs may leave them unset).

cd /src
# --locked: the source is mounted read-only, so Cargo.lock must not be rewritten.
# Shipped binary: a plain build (no extra features) so it's byte-identical to a normal release build.
cargo build --release --locked --bin crowd-cast-agent
# Offline manifest signer: built here too (behind release-tools) so the CI host needs no Rust, no
# GTK3, and no API-gateway env just to sign. A glibc-2.34 signer runs fine on the newer-glibc host.
cargo build --release --locked --features release-tools --bin cc-sign-manifest

TGT="${CARGO_TARGET_DIR:-/src/target}/release"
install -Dm755 "$TGT/crowd-cast-agent" /out/crowd-cast-agent-x86_64
install -Dm755 "$TGT/cc-sign-manifest" /out/cc-sign-manifest

echo "=== glibc floor (highest GLIBC_x.y required; must be <= 2.34) ==="
objdump -T /out/crowd-cast-agent-x86_64 2>/dev/null | grep -oE 'GLIBC_[0-9]+\.[0-9]+' | sort -uV | tail -5
echo "=== DT_NEEDED (host-provided libs only; libobs resolves via RUNPATH, not NEEDED-at-load) ==="
objdump -p /out/crowd-cast-agent-x86_64 | awk '/NEEDED/{print "  "$2}'
echo "=== RUNPATH (self-provisioning path to the installed bundle) ==="
objdump -p /out/crowd-cast-agent-x86_64 | awk '/RUNPATH|RPATH/{print "  "$2}'
echo "OK -> /out/crowd-cast-agent-x86_64"
