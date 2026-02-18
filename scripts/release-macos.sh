#!/bin/bash
# End-to-end macOS release workflow:
# build/sign app, create/sign dmg, notarize, staple, and verify.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_ROOT"

APP_NAME="CrowdCast"
BUILD_TYPE="release"
TARGET_DIR="target/release"
APP_PATH="${TARGET_DIR}/${APP_NAME}.app"
DMG_PATH="${TARGET_DIR}/${APP_NAME}.dmg"

SIGN_IDENTITY="${CROWD_CAST_MACOS_SIGN_IDENTITY:-}"
NOTARY_PROFILE="${CROWD_CAST_NOTARY_PROFILE:-crowdcast-notary}"
API_GATEWAY_URL="${CROWD_CAST_API_GATEWAY_URL:-}"
NOTARIZE=1

usage() {
    cat <<EOF
Usage: scripts/release-macos.sh [options]

Options:
  --identity "<identity>"      Developer ID Application identity
  --notary-profile <profile>   notarytool profile (default: crowdcast-notary)
  --api-gateway-url <url>      Build-time CROWD_CAST_API_GATEWAY_URL
  --debug                      Use debug build artifacts
  --skip-notarize              Skip notarization/stapling steps
  -h, --help                   Show this help

Environment fallbacks:
  CROWD_CAST_MACOS_SIGN_IDENTITY
  CROWD_CAST_NOTARY_PROFILE
  CROWD_CAST_API_GATEWAY_URL
EOF
}

require_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "Missing required command: $1" >&2
        exit 1
    fi
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --identity)
            SIGN_IDENTITY="$2"
            shift 2
            ;;
        --notary-profile)
            NOTARY_PROFILE="$2"
            shift 2
            ;;
        --api-gateway-url)
            API_GATEWAY_URL="$2"
            shift 2
            ;;
        --debug)
            BUILD_TYPE="debug"
            TARGET_DIR="target/debug"
            APP_PATH="${TARGET_DIR}/${APP_NAME}.app"
            DMG_PATH="${TARGET_DIR}/${APP_NAME}.dmg"
            shift
            ;;
        --skip-notarize)
            NOTARIZE=0
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

require_cmd cargo
require_cmd codesign
require_cmd spctl
require_cmd hdiutil

if [[ -z "$SIGN_IDENTITY" ]]; then
    echo "Missing signing identity. Pass --identity or set CROWD_CAST_MACOS_SIGN_IDENTITY." >&2
    exit 1
fi

if [[ -z "$API_GATEWAY_URL" ]]; then
    echo "Missing API gateway URL. Pass --api-gateway-url or set CROWD_CAST_API_GATEWAY_URL." >&2
    exit 1
fi

echo "Step 1/5: Build and sign app bundle..."
if [[ "$BUILD_TYPE" == "debug" ]]; then
    CROWD_CAST_API_GATEWAY_URL="$API_GATEWAY_URL" scripts/bundle-macos.sh --debug --identity "$SIGN_IDENTITY" --no-verify
else
    CROWD_CAST_API_GATEWAY_URL="$API_GATEWAY_URL" scripts/bundle-macos.sh --identity "$SIGN_IDENTITY" --no-verify
fi

if [[ ! -d "$APP_PATH" ]]; then
    echo "Expected app bundle not found: $APP_PATH" >&2
    exit 1
fi

echo "Step 2/5: Create DMG..."
rm -f "$DMG_PATH"
hdiutil create \
    -volname "$APP_NAME" \
    -srcfolder "$APP_PATH" \
    -ov \
    -format UDZO \
    "$DMG_PATH"

echo "Step 3/5: Sign DMG..."
codesign --force --timestamp --sign "$SIGN_IDENTITY" "$DMG_PATH"

if [[ "$NOTARIZE" -eq 1 ]]; then
    require_cmd xcrun
    echo "Step 4/5: Notarize DMG and staple artifacts..."
    xcrun notarytool submit "$DMG_PATH" --keychain-profile "$NOTARY_PROFILE" --wait
    xcrun stapler staple "$APP_PATH"
    xcrun stapler staple "$DMG_PATH"
else
    echo "Step 4/5: Skipped notarization/stapling (--skip-notarize)."
fi

echo "Step 5/5: Verification gates..."
codesign --verify --strict --deep --verbose=2 "$APP_PATH"
if [[ "$NOTARIZE" -eq 1 ]]; then
    spctl --assess --type execute --verbose "$APP_PATH"
    spctl --assess --type open --verbose "$DMG_PATH"
else
    echo "Skipping Gatekeeper assessment because notarization was skipped."
fi

echo
echo "Release artifacts:"
echo "  App: $APP_PATH"
echo "  DMG: $DMG_PATH"
