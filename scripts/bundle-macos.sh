#!/bin/bash
# Bundle crowd-cast-agent as a macOS .app bundle and sign with Developer ID.
set -euo pipefail

APP_NAME="CrowdCast"
BINARY_NAME="crowd-cast-agent"
SIGN_IDENTITY="${CROWD_CAST_MACOS_SIGN_IDENTITY:-}"
ENTITLEMENTS_PATH="resources/macos/Entitlements.plist"
SKIP_SIGN=0
VERIFY_SIGN=1
BUILD_TYPE="release"
APP_VERSION="${CROWD_CAST_APP_VERSION:-}"
BUILD_NUMBER="${CROWD_CAST_BUILD_NUMBER:-}"
SPARKLE_FEED_URL="${CROWD_CAST_SPARKLE_FEED_URL:-}"
SPARKLE_PUBLIC_ED_KEY="${CROWD_CAST_SPARKLE_PUBLIC_ED_KEY:-}"
SPARKLE_AUTO_CHECKS=1
SPARKLE_AUTO_UPDATE=1

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

usage() {
    cat <<EOF
Usage: scripts/bundle-macos.sh [options]

Options:
  --debug                      Build debug binary
  --identity "<identity>"      Developer ID Application identity
  --entitlements <plist>       Entitlements plist path
  --version <semver>           CFBundleShortVersionString (defaults to Cargo.toml version)
  --build-number <number>      CFBundleVersion (defaults to UTC timestamp)
  --feed-url <url>             Sparkle appcast feed URL
  --sparkle-public-ed-key <k>  Sparkle SUPublicEDKey value
  --disable-auto-checks        Set SUEnableAutomaticChecks to false
  --enable-auto-update         Set SUAutomaticallyUpdate to true
  --skip-sign                  Skip code signing
  --no-verify                  Skip codesign/spctl verification
  -h, --help                   Show this help
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --debug)
            BUILD_TYPE="debug"
            shift
            ;;
        --identity)
            SIGN_IDENTITY="$2"
            shift 2
            ;;
        --entitlements)
            ENTITLEMENTS_PATH="$2"
            shift 2
            ;;
        --version)
            APP_VERSION="$2"
            shift 2
            ;;
        --build-number)
            BUILD_NUMBER="$2"
            shift 2
            ;;
        --feed-url)
            SPARKLE_FEED_URL="$2"
            shift 2
            ;;
        --sparkle-public-ed-key)
            SPARKLE_PUBLIC_ED_KEY="$2"
            shift 2
            ;;
        --disable-auto-checks)
            SPARKLE_AUTO_CHECKS=0
            shift
            ;;
        --enable-auto-update)
            SPARKLE_AUTO_UPDATE=1
            shift
            ;;
        --skip-sign)
            SKIP_SIGN=1
            shift
            ;;
        --no-verify)
            VERIFY_SIGN=0
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            usage
            exit 1
            ;;
    esac
done

cd "$PROJECT_ROOT"

default_version() {
    sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1
}

plist_set_string() {
    local plist_path="$1"
    local key="$2"
    local value="$3"
    /usr/libexec/PlistBuddy -c "Set :${key} ${value}" "$plist_path" >/dev/null 2>&1 \
        || /usr/libexec/PlistBuddy -c "Add :${key} string ${value}" "$plist_path" >/dev/null
}

plist_set_bool() {
    local plist_path="$1"
    local key="$2"
    local value="$3"
    /usr/libexec/PlistBuddy -c "Set :${key} ${value}" "$plist_path" >/dev/null 2>&1 \
        || /usr/libexec/PlistBuddy -c "Add :${key} bool ${value}" "$plist_path" >/dev/null
}

plist_set_int() {
    local plist_path="$1"
    local key="$2"
    local value="$3"
    /usr/libexec/PlistBuddy -c "Set :${key} ${value}" "$plist_path" >/dev/null 2>&1 \
        || /usr/libexec/PlistBuddy -c "Add :${key} integer ${value}" "$plist_path" >/dev/null
}

plist_delete_key() {
    local plist_path="$1"
    local key="$2"
    /usr/libexec/PlistBuddy -c "Delete :${key}" "$plist_path" >/dev/null 2>&1 || true
}

sign_file() {
    local path="$1"
    codesign --force --timestamp --options runtime --sign "$SIGN_IDENTITY" "$path"
}

APP_VERSION="${APP_VERSION:-$(default_version)}"
BUILD_NUMBER="${BUILD_NUMBER:-$(date -u +%Y%m%d%H%M%S)}"

if [[ -z "${CROWD_CAST_SKIP_SPARKLE:-}" ]]; then
    "$PROJECT_ROOT/scripts/fetch-sparkle.sh" >/dev/null
fi

echo "Building $BUILD_TYPE binary..."
if [[ "$BUILD_TYPE" == "release" ]]; then
    cargo build --release
else
    cargo build
fi

TARGET_DIR="target/${BUILD_TYPE}"
APP_DIR="${TARGET_DIR}/${APP_NAME}.app"
APP_EXEC="$APP_DIR/Contents/MacOS/${BINARY_NAME}"

echo "Creating app bundle at: $APP_DIR"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources" "$APP_DIR/Contents/Frameworks"

cp "${TARGET_DIR}/${BINARY_NAME}" "$APP_EXEC"
cp "resources/macos/Info.plist" "$APP_DIR/Contents/"

INFO_PLIST="$APP_DIR/Contents/Info.plist"
plist_set_string "$INFO_PLIST" "CFBundleShortVersionString" "$APP_VERSION"
plist_set_string "$INFO_PLIST" "CFBundleVersion" "$BUILD_NUMBER"
plist_set_bool "$INFO_PLIST" "SUEnableAutomaticChecks" "$( [[ "$SPARKLE_AUTO_CHECKS" -eq 1 ]] && echo true || echo false )"
plist_set_bool "$INFO_PLIST" "SUAutomaticallyUpdate" "$( [[ "$SPARKLE_AUTO_UPDATE" -eq 1 ]] && echo true || echo false )"
plist_set_int "$INFO_PLIST" "SUScheduledCheckInterval" "60"

if [[ -n "$SPARKLE_FEED_URL" ]]; then
    plist_set_string "$INFO_PLIST" "SUFeedURL" "$SPARKLE_FEED_URL"
else
    plist_delete_key "$INFO_PLIST" "SUFeedURL"
fi

if [[ -n "$SPARKLE_PUBLIC_ED_KEY" ]]; then
    plist_set_string "$INFO_PLIST" "SUPublicEDKey" "$SPARKLE_PUBLIC_ED_KEY"
else
    plist_delete_key "$INFO_PLIST" "SUPublicEDKey"
fi

if [[ -f "resources/macos/AppIcon.icns" ]]; then
    cp "resources/macos/AppIcon.icns" "$APP_DIR/Contents/Resources/"
fi

if [[ -f "assets/logo.png" ]]; then
    cp "assets/logo.png" "$APP_DIR/Contents/Resources/"
fi

# Bundle minimal OBS runtime needed for dyld launch/linking:
# - libobs.framework (directly linked by crowd-cast-agent)
# - all OBS-provided dylibs (ffmpeg and related deps required by libobs)
if [[ -d "${TARGET_DIR}/libobs.framework" ]]; then
    cp -R "${TARGET_DIR}/libobs.framework" "$APP_DIR/Contents/Frameworks/"
else
    echo "Missing required framework: ${TARGET_DIR}/libobs.framework" >&2
    exit 1
fi

for dylib in "${TARGET_DIR}"/*.dylib; do
    if [[ -f "$dylib" ]]; then
        cp "$dylib" "$APP_DIR/Contents/Frameworks/"
    fi
done

SPARKLE_DIR="${CROWD_CAST_SPARKLE_DIR:-$PROJECT_ROOT/build/sparkle/${CROWD_CAST_SPARKLE_VERSION:-2.8.1}}"
if [[ -z "${CROWD_CAST_SKIP_SPARKLE:-}" && -d "$SPARKLE_DIR/Sparkle.framework" ]]; then
    cp -R "$SPARKLE_DIR/Sparkle.framework" "$APP_DIR/Contents/Frameworks/"
fi

echo "Bundled libobs loader runtime into Frameworks (plugins/data remain external)."

echo "Updating binary rpaths..."
install_name_tool -add_rpath "@executable_path/../Frameworks" "$APP_EXEC" 2>/dev/null || true

if [[ "$SKIP_SIGN" -eq 0 ]]; then
    if [[ -z "$SIGN_IDENTITY" ]]; then
        echo "Signing requested but no identity provided." >&2
        echo "Pass --identity or set CROWD_CAST_MACOS_SIGN_IDENTITY." >&2
        exit 1
    fi

    if [[ ! -f "$ENTITLEMENTS_PATH" ]]; then
        echo "Entitlements file not found: $ENTITLEMENTS_PATH" >&2
        exit 1
    fi

    echo "Signing standalone dylibs..."
    while IFS= read -r -d '' file; do
        sign_file "$file"
    done < <(find "$APP_DIR/Contents/Frameworks" -maxdepth 1 -type f -name "*.dylib" -print0 2>/dev/null || true)

    if [[ -d "$APP_DIR/Contents/Frameworks/Sparkle.framework" ]]; then
        echo "Signing Sparkle helper bundles..."
        while IFS= read -r -d '' helper; do
            sign_file "$helper"
        done < <(find "$APP_DIR/Contents/Frameworks/Sparkle.framework" \( -type d -name "*.xpc" -o -type d -name "*.app" \) -print0 2>/dev/null || true)

        echo "Signing Sparkle standalone executables..."
        while IFS= read -r -d '' exe; do
            if file -b "$exe" | grep -q "Mach-O"; then
                sign_file "$exe"
            fi
        done < <(find "$APP_DIR/Contents/Frameworks/Sparkle.framework" -type f ! -name "*.dylib" -print0 2>/dev/null || true)
    fi

    echo "Signing frameworks..."
    while IFS= read -r -d '' framework; do
        sign_file "$framework"
    done < <(find "$APP_DIR/Contents/Frameworks" -maxdepth 1 -type d -name "*.framework" -print0 2>/dev/null || true)

    echo "Signing main executable..."
    sign_file "$APP_EXEC"

    echo "Signing app bundle..."
    codesign \
        --force \
        --timestamp \
        --options runtime \
        --entitlements "$ENTITLEMENTS_PATH" \
        --sign "$SIGN_IDENTITY" \
        "$APP_DIR"
else
    echo "Skipping code signing (--skip-sign)."
fi

if [[ "$SKIP_SIGN" -eq 0 && "$VERIFY_SIGN" -eq 1 ]]; then
    echo "Verifying signature..."
    codesign --verify --strict --deep --verbose=2 "$APP_DIR"
    spctl --assess --type execute --verbose "$APP_DIR"
fi

echo
echo "Successfully created: $APP_DIR"
echo "Bundle contents:"
du -sh "$APP_DIR/Contents/MacOS" 2>/dev/null || true
du -sh "$APP_DIR/Contents/Frameworks" 2>/dev/null || true
du -sh "$APP_DIR/Contents/Resources" 2>/dev/null || true
