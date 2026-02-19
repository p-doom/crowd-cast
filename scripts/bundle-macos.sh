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

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

usage() {
    cat <<EOF
Usage: scripts/bundle-macos.sh [options]

Options:
  --debug                      Build debug binary
  --identity "<identity>"      Developer ID Application identity
  --entitlements <plist>       Entitlements plist path
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

echo "Bundled libobs loader runtime into Frameworks (plugins/data remain external)."

echo "Updating binary rpaths..."
install_name_tool -add_rpath "@executable_path/../Frameworks" "$APP_EXEC" 2>/dev/null || true

sign_file() {
    local path="$1"
    codesign --force --timestamp --options runtime --sign "$SIGN_IDENTITY" "$path"
}

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
