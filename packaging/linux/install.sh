#!/usr/bin/env bash
# crowd-cast Linux installer — no-root, user-space install into ~/.local.
#
# Lays out exactly what the self-provisioning binary expects (see
# src/capture/context.rs):
#   ~/.local/bin/crowd-cast-agent                      (the binary; RUNPATH-relative to the bundle)
#   ~/.local/share/crowd-cast/obs/<abi>/usr/...        (the relocatable libobs bundle)
#   ~/.local/share/applications/crowd-cast.desktop     (menu entry)
#   ~/.local/share/icons/hicolor/256x256/apps/crowd-cast.png
# plus the ONE privileged step: ensure the user is in the `input` group (evdev capture).
#
# Usage (production):
#   curl -fsSL https://github.com/p-doom/crowd-cast/releases/latest/download/install-linux.sh | bash
# Usage (dev channel):
#   curl -fsSL https://raw.githubusercontent.com/p-doom/crowd-cast/linux-compat/packaging/linux/install.sh | bash -s -- --channel dev
# Usage (dev/test, from a checkout):
#   packaging/linux/install.sh --local
#   packaging/linux/install.sh --uninstall
#
# Design law (memory: crowd-cast-no-fallbacks): verify prerequisites and FAIL CLOSED with an
# actionable message — never silently degrade.
set -euo pipefail

APP="crowd-cast-agent"
PREFIX="${CROWD_CAST_PREFIX:-$HOME/.local}"
OBS_ABI="${CROWD_CAST_OBS_ABI:-}"
BASE_URL="${CROWD_CAST_RELEASE_BASE_URL:-}"
FEED_URL="${CROWD_CAST_RELEASE_FEED_URL:-}"
CHANNEL="${CROWD_CAST_RELEASE_CHANNEL:-prod}"
MODE="remote"
DO_UNINSTALL=0
ABI_EXPLICIT=0
CHANNEL_EXPLICIT=0

# The release workflow replaces these placeholders in the uploaded install-linux.sh asset. A raw
# checkout copy still works by deriving the feed from --channel; it just cannot verify the appcast
# signature unless CROWD_CAST_UPDATE_PUBKEY is supplied by the caller.
FEED_PLACEHOLDER="__CROWD_CAST_""DEFAULT_FEED_URL__"
PUBKEY_PLACEHOLDER="__CROWD_CAST_""UPDATE_PUBKEY__"
EMBEDDED_FEED_URL="__CROWD_CAST_DEFAULT_FEED_URL__"
EMBEDDED_PUBKEY="__CROWD_CAST_UPDATE_PUBKEY__"
[ "$EMBEDDED_FEED_URL" = "$FEED_PLACEHOLDER" ] && EMBEDDED_FEED_URL=""
[ "$EMBEDDED_PUBKEY" = "$PUBKEY_PLACEHOLDER" ] && EMBEDDED_PUBKEY=""
APPCAST_PUBKEY="${CROWD_CAST_UPDATE_PUBKEY:-$EMBEDDED_PUBKEY}"

BIN_DIR="$PREFIX/bin"
SHARE_DIR="$PREFIX/share/crowd-cast"
APPS_DIR="$PREFIX/share/applications"
ICON_DIR="$PREFIX/share/icons/hicolor/256x256/apps"

# Resolve the repo root when run as a file (needed only for --local).
SCRIPT_SOURCE="${BASH_SOURCE[0]:-$0}"
REPO_ROOT=""
if [ -f "$SCRIPT_SOURCE" ]; then
    REPO_ROOT="$(cd "$(dirname "$SCRIPT_SOURCE")/../.." && pwd)"
fi

err()  { echo "error: $*" >&2; exit 1; }
info() { echo ">> $*"; }

usage() {
    sed -n '2,30p' "$SCRIPT_SOURCE" 2>/dev/null | sed 's/^# \{0,1\}//'
    cat <<EOF

Options:
  --local            Install from this checkout's build outputs (target/release + packaging/linux/out)
  --uninstall        Remove the user-space install (keeps config + the input-group membership)
  --prefix <dir>     Install prefix (default: \$HOME/.local)
  --channel <name>   Release channel: prod or dev (default: ${CHANNEL:-prod})
  --feed-url <url>   Linux appcast URL (or CROWD_CAST_RELEASE_FEED_URL)
  --abi <ver>        Require a specific libobs bundle ABI (or CROWD_CAST_OBS_ABI)
  --base-url <url>   Legacy flat release base URL (or CROWD_CAST_RELEASE_BASE_URL)
  -h, --help         Show this help
EOF
}

[ -n "${CROWD_CAST_RELEASE_CHANNEL:-}" ] && CHANNEL_EXPLICIT=1
[ -n "${CROWD_CAST_OBS_ABI:-}" ] && ABI_EXPLICIT=1

while [ $# -gt 0 ]; do
    case "$1" in
        --local)     MODE="local"; shift ;;
        --uninstall) DO_UNINSTALL=1; shift ;;
        --prefix)    PREFIX="$2"; BIN_DIR="$PREFIX/bin"; SHARE_DIR="$PREFIX/share/crowd-cast"; APPS_DIR="$PREFIX/share/applications"; ICON_DIR="$PREFIX/share/icons/hicolor/256x256/apps"; shift 2 ;;
        --channel)   CHANNEL="$2"; CHANNEL_EXPLICIT=1; shift 2 ;;
        --feed-url)  FEED_URL="$2"; shift 2 ;;
        --abi)       OBS_ABI="$2"; ABI_EXPLICIT=1; shift 2 ;;
        --base-url)  BASE_URL="$2"; shift 2 ;;
        -h|--help)   usage; exit 0 ;;
        *)           err "unknown option: $1 (see --help)" ;;
    esac
done

# ---- uninstall ------------------------------------------------------------
if [ "$DO_UNINSTALL" -eq 1 ]; then
    info "Removing crowd-cast user-space install..."
    rm -f  "$BIN_DIR/$APP"
    rm -rf "$SHARE_DIR"
    rm -f  "$APPS_DIR/crowd-cast.desktop"
    rm -f  "$ICON_DIR/crowd-cast.png"
    rm -f  "${XDG_CONFIG_HOME:-$HOME/.config}/autostart/crowd-cast.desktop"
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APPS_DIR" 2>/dev/null || true
    info "Done. Left untouched: your config/data and your 'input' group membership."
    exit 0
fi

# ---- prerequisites (fail closed) -----------------------------------------
[ "$(uname -s)" = "Linux" ]   || err "this installer is for Linux only (got $(uname -s))."
[ "$(uname -m)" = "x86_64" ]  || err "only x86_64 is published right now (got $(uname -m))."
command -v tar  >/dev/null 2>&1 || err "'tar' is required."
command -v zstd >/dev/null 2>&1 || err "'zstd' is required (install it: e.g. apt install zstd / pacman -S zstd)."
command -v sha256sum >/dev/null 2>&1 || err "'sha256sum' is required."
command -v python3 >/dev/null 2>&1 || err "'python3' is required."

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

fetch() {  # fetch <url> <dest>
    if command -v curl >/dev/null 2>&1; then curl -fSL "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then wget -qO "$2" "$1"
    else err "need 'curl' or 'wget' to download."; fi
}

sha256_first() { sha256sum "$1" | awk '{print $1}'; }

verify_sha256() {  # verify_sha256 <file> <expected-hex>  (fail closed)
    local got exp; got="$(sha256_first "$1")"; exp="$(echo "$2" | awk '{print $1}')"
    [ -n "$exp" ] || err "missing expected checksum for $(basename "$1")."
    [ "$got" = "$exp" ] || err "checksum mismatch for $(basename "$1"): expected $exp, got $got."
}

manifest_field() {  # manifest_field <manifest> <top-level-key> <nested-key>
    python3 - "$1" "$2" "$3" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as f:
    manifest = json.load(f)
value = manifest[sys.argv[2]][sys.argv[3]]
if not isinstance(value, str) or not value:
    raise SystemExit(f"manifest field {sys.argv[2]}.{sys.argv[3]} is missing or not a string")
print(value)
PY
}

verify_appcast_signature() {  # verify_appcast_signature <manifest> <sig-b64-file> <pubkey-b64>
    local manifest="$1" sig_b64="$2" pubkey_b64="$3"
    local msg="$WORK/appcast-message" pub_der="$WORK/appcast-pubkey.der" sig_raw="$WORK/appcast.sig.raw"
    command -v openssl >/dev/null 2>&1 || err "'openssl' is required to verify the signed Linux appcast."
    python3 - "$manifest" "$sig_b64" "$pubkey_b64" "$msg" "$pub_der" "$sig_raw" <<'PY'
import base64, pathlib, sys
manifest, sig_path, pubkey_b64, msg_path, pub_der_path, sig_raw_path = sys.argv[1:]
prefix = b"crowd-cast/linux-appcast/v1\n"
pubkey = base64.b64decode(pubkey_b64.strip(), validate=True)
if len(pubkey) != 32:
    raise SystemExit(f"update public key must decode to 32 bytes, got {len(pubkey)}")
signature = base64.b64decode(pathlib.Path(sig_path).read_text().strip(), validate=True)
if len(signature) != 64:
    raise SystemExit(f"appcast signature must decode to 64 bytes, got {len(signature)}")
pathlib.Path(msg_path).write_bytes(prefix + pathlib.Path(manifest).read_bytes())
pathlib.Path(pub_der_path).write_bytes(bytes.fromhex("302a300506032b6570032100") + pubkey)
pathlib.Path(sig_raw_path).write_bytes(signature)
PY
    openssl pkeyutl -verify -pubin -keyform DER -inkey "$pub_der" -rawin -in "$msg" -sigfile "$sig_raw" >/dev/null
}

feed_for_channel() {
    case "$1" in
        prod) echo "https://crowd-cast-bucket.s3.amazonaws.com/appcast-linux.json" ;;
        dev)  echo "https://crowd-cast-bucket.s3.amazonaws.com/appcast-linux-dev.json" ;;
        *)    err "unknown release channel '$1' (expected prod or dev)" ;;
    esac
}

# ---- resolve + verify sources --------------------------------------------
BIN_SRC="$WORK/$APP"

if [ "$MODE" = "local" ]; then
    OBS_ABI="${OBS_ABI:-32.0.2}"
    BUNDLE_SRC="$WORK/obs-bundle-$OBS_ABI-x86_64.tar.zst"
    [ -n "$REPO_ROOT" ] || err "--local must be run from a checkout (could not resolve repo root)."
    local_bin="$REPO_ROOT/target/release/$APP"
    local_bundle="$REPO_ROOT/packaging/linux/out/obs-bundle-$OBS_ABI-x86_64.tar.zst"
    [ -x "$local_bin" ]     || err "local binary not found: $local_bin (build it: cargo build --release)."
    [ -f "$local_bundle" ]  || err "local bundle not found: $local_bundle (build it: packaging/linux/run-build.sh)."
    info "Local install from $REPO_ROOT"
    cp "$local_bin" "$BIN_SRC"
    cp "$local_bundle" "$BUNDLE_SRC"
    # Verify the bundle against its sidecar checksum if present (integrity of the built artifact).
    if [ -f "$local_bundle.sha256" ]; then verify_sha256 "$BUNDLE_SRC" "$(cat "$local_bundle.sha256")"; info "bundle checksum OK"; fi
elif [ -n "$BASE_URL" ]; then
    OBS_ABI="${OBS_ABI:-32.0.2}"
    BUNDLE_SRC="$WORK/obs-bundle-$OBS_ABI-x86_64.tar.zst"
    BASE_URL="${BASE_URL%/}"
    info "Downloading from legacy release base $BASE_URL ..."
    fetch "$BASE_URL/$APP-x86_64"                          "$BIN_SRC"
    fetch "$BASE_URL/$APP-x86_64.sha256"                   "$WORK/bin.sha256"
    fetch "$BASE_URL/obs-bundle-$OBS_ABI-x86_64.tar.zst"   "$BUNDLE_SRC"
    fetch "$BASE_URL/obs-bundle-$OBS_ABI-x86_64.tar.zst.sha256" "$WORK/bundle.sha256"
    verify_sha256 "$BIN_SRC"    "$(cat "$WORK/bin.sha256")"
    verify_sha256 "$BUNDLE_SRC" "$(cat "$WORK/bundle.sha256")"
    info "checksums OK"
else
    if [ -z "$FEED_URL" ]; then
        if [ "$CHANNEL_EXPLICIT" -eq 0 ] && [ -n "$EMBEDDED_FEED_URL" ]; then
            FEED_URL="$EMBEDDED_FEED_URL"
        else
            FEED_URL="$(feed_for_channel "$CHANNEL")"
        fi
    fi

    MANIFEST="$WORK/appcast-linux.json"
    SIG="$WORK/appcast-linux.json.sig"
    info "Downloading Linux appcast from $FEED_URL ..."
    fetch "$FEED_URL" "$MANIFEST"

    if [ -n "$APPCAST_PUBKEY" ]; then
        fetch "$FEED_URL.sig" "$SIG"
        verify_appcast_signature "$MANIFEST" "$SIG" "$APPCAST_PUBKEY"
        info "appcast signature OK"
    else
        info "appcast signature skipped (no CROWD_CAST_UPDATE_PUBKEY embedded or supplied); artifact hashes are still checked"
    fi

    manifest_abi="$(manifest_field "$MANIFEST" bundle abi)"
    if [ "$ABI_EXPLICIT" -eq 1 ] && [ "$OBS_ABI" != "$manifest_abi" ]; then
        err "manifest bundle ABI is $manifest_abi, but $OBS_ABI was requested"
    fi
    OBS_ABI="$manifest_abi"
    BUNDLE_SRC="$WORK/obs-bundle-$OBS_ABI-x86_64.tar.zst"

    binary_url="$(manifest_field "$MANIFEST" binary url)"
    binary_sha="$(manifest_field "$MANIFEST" binary sha256)"
    bundle_url="$(manifest_field "$MANIFEST" bundle url)"
    bundle_sha="$(manifest_field "$MANIFEST" bundle sha256)"

    info "Downloading crowd-cast binary..."
    fetch "$binary_url" "$BIN_SRC"
    verify_sha256 "$BIN_SRC" "$binary_sha"
    info "Downloading libobs bundle ABI $OBS_ABI..."
    fetch "$bundle_url" "$BUNDLE_SRC"
    verify_sha256 "$BUNDLE_SRC" "$bundle_sha"
    info "checksums OK"
fi

BUNDLE_DIR="$SHARE_DIR/obs/$OBS_ABI"

# ---- install binary (atomic rename) --------------------------------------
mkdir -p "$BIN_DIR"
install -m 0755 "$BIN_SRC" "$BIN_DIR/$APP.new"
mv -f "$BIN_DIR/$APP.new" "$BIN_DIR/$APP"
info "installed binary -> $BIN_DIR/$APP"

# ---- install bundle (atomic dir swap) ------------------------------------
mkdir -p "$SHARE_DIR/obs"
STAGE="$BUNDLE_DIR.stage.$$"
rm -rf "$STAGE"; mkdir -p "$STAGE"
tar --zstd -xf "$BUNDLE_SRC" -C "$STAGE"
[ -f "$STAGE/usr/share/obs/libobs/default.effect" ] || err "bundle looks malformed (no usr/share/obs/libobs/default.effect)."
rm -rf "$BUNDLE_DIR"
mv "$STAGE" "$BUNDLE_DIR"
info "installed libobs bundle -> $BUNDLE_DIR (ABI $OBS_ABI)"

# ---- the one privileged step: 'input' group for evdev capture ------------
if id -nG 2>/dev/null | tr ' ' '\n' | grep -qx input; then
    info "'input' group: already a member"
else
    info "Adding you to the 'input' group (needs sudo; required for input capture)..."
    if command -v sudo >/dev/null 2>&1 && sudo usermod -aG input "$USER"; then
        NEED_RELOGIN=1
        info "added to 'input' group"
    else
        err "could not add you to the 'input' group. Run manually, then re-login:
       sudo usermod -aG input \"$USER\""
    fi
fi

# ---- desktop integration (menu entry + icon) -----------------------------
mkdir -p "$APPS_DIR" "$ICON_DIR"
ICON_LINE=""
ICON_SRC=""
[ "$MODE" = "local" ] && ICON_SRC="$REPO_ROOT/assets/logo.png"
if [ "$MODE" != "local" ] && [ -n "${binary_url:-}" ]; then
    ASSET_BASE="${binary_url%/*}"
    fetch "$ASSET_BASE/logo.png" "$WORK/logo.png" 2>/dev/null && ICON_SRC="$WORK/logo.png" || true
elif [ "$MODE" != "local" ] && [ -n "$BASE_URL" ]; then
    fetch "$BASE_URL/logo.png" "$WORK/logo.png" 2>/dev/null && ICON_SRC="$WORK/logo.png" || true
fi
if [ -n "$ICON_SRC" ] && [ -f "$ICON_SRC" ]; then
    install -m 0644 "$ICON_SRC" "$ICON_DIR/crowd-cast.png"
    ICON_LINE="Icon=crowd-cast"
fi
cat > "$APPS_DIR/crowd-cast.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=crowd-cast
Comment=crowd-cast data collection agent
Exec=$BIN_DIR/$APP
$ICON_LINE
Terminal=false
Categories=Utility;
StartupNotify=false
EOF
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APPS_DIR" 2>/dev/null || true
info "installed menu entry -> $APPS_DIR/crowd-cast.desktop"

# ---- final guidance -------------------------------------------------------
echo
info "crowd-cast installed."
case ":$PATH:" in
    *":$BIN_DIR:"*) : ;;
    *) echo "   note: $BIN_DIR is not on your PATH (fine for the menu launcher; add it to run '$APP' in a terminal)." ;;
esac
if [ "${NEED_RELOGIN:-0}" = "1" ]; then
    echo "   IMPORTANT: log out and back in for 'input' group membership to take effect, then launch crowd-cast."
else
    echo "   Launch it from your app menu, or run: $BIN_DIR/$APP"
fi
