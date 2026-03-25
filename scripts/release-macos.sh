#!/bin/bash
# End-to-end macOS release workflow:
# build/sign app, create/sign dmg, and optionally notarize/staple.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_ROOT"

APP_NAME="CrowdCast"
BUILD_TYPE="release"
TARGET_DIR="target/release"
APP_PATH="${TARGET_DIR}/${APP_NAME}.app"
DMG_PATH="${TARGET_DIR}/${APP_NAME}.dmg"
SPARKLE_ARCHIVE_DIR="${TARGET_DIR}/sparkle"
SPARKLE_ARCHIVE_PATH=""

SIGN_IDENTITY="${CROWD_CAST_MACOS_SIGN_IDENTITY:-}"
NOTARY_PROFILE="${CROWD_CAST_NOTARY_PROFILE:-crowdcast-notary}"
API_GATEWAY_URL="${CROWD_CAST_API_GATEWAY_URL:-}"
DMG_BACKGROUND="resources/macos/dmg-background@2x.png"
NOTARIZE=0
APP_VERSION="${CROWD_CAST_APP_VERSION:-}"
BUILD_NUMBER="${CROWD_CAST_BUILD_NUMBER:-}"
SPARKLE_FEED_URL="${CROWD_CAST_SPARKLE_FEED_URL:-}"
SPARKLE_PUBLIC_ED_KEY="${CROWD_CAST_SPARKLE_PUBLIC_ED_KEY:-}"
SPARKLE_ARCHIVE_BASE_URL="${CROWD_CAST_SPARKLE_ARCHIVE_BASE_URL:-}"
SPARKLE_PRIVATE_ED_KEY_FILE="${CROWD_CAST_SPARKLE_PRIVATE_ED_KEY_FILE:-}"
SPARKLE_RELEASE_NOTES_URL_PREFIX="${CROWD_CAST_SPARKLE_RELEASE_NOTES_URL_PREFIX:-}"
SPARKLE_FULL_RELEASE_NOTES_URL="${CROWD_CAST_SPARKLE_FULL_RELEASE_NOTES_URL:-}"
SPARKLE_PRODUCT_LINK="${CROWD_CAST_SPARKLE_PRODUCT_LINK:-}"
SPARKLE_CHANNEL="${CROWD_CAST_SPARKLE_CHANNEL:-}"
SPARKLE_PHASED_ROLLOUT_INTERVAL="${CROWD_CAST_SPARKLE_PHASED_ROLLOUT_INTERVAL:-}"
SPARKLE_CRITICAL_UPDATE_VERSION="${CROWD_CAST_SPARKLE_CRITICAL_UPDATE_VERSION:-}"
GENERATE_APPCAST=0

usage() {
    cat <<EOF
Usage: scripts/release-macos.sh [options]

Options:
  --identity "<identity>"      Developer ID Application identity
  --notary-profile <profile>   notarytool profile (default: crowdcast-notary)
  --api-gateway-url <url>      Build-time CROWD_CAST_API_GATEWAY_URL
  --version <semver>           CFBundleShortVersionString
  --build-number <number>      CFBundleVersion
  --feed-url <url>             Sparkle SUFeedURL value
  --sparkle-public-ed-key <k>  Sparkle SUPublicEDKey value
  --sparkle-archive-base-url <url>
                               Base URL where Sparkle ZIP archives will be hosted
  --sparkle-private-ed-key-file <file>
                               Private EdDSA key for generate_appcast
  --sparkle-release-notes-url-prefix <url>
                               Base URL for release notes sidecar files
  --sparkle-full-release-notes-url <url>
                               Full release notes page URL
  --sparkle-product-link <url> Product website URL for Sparkle items
  --sparkle-channel <name>     Sparkle channel for new appcast items
  --sparkle-phased-rollout-interval <secs>
                               Enable phased rollout for newly generated updates
  --sparkle-critical-update-version <ver>
                               Mark the new update as critical for older versions
  --debug                      Use debug build artifacts
  --notarize                   Run notarization and stapling steps
  -h, --help                   Show this help

Environment fallbacks:
  CROWD_CAST_MACOS_SIGN_IDENTITY
  CROWD_CAST_NOTARY_PROFILE
  CROWD_CAST_API_GATEWAY_URL
  CROWD_CAST_APP_VERSION
  CROWD_CAST_BUILD_NUMBER
  CROWD_CAST_SPARKLE_FEED_URL
  CROWD_CAST_SPARKLE_PUBLIC_ED_KEY
  CROWD_CAST_SPARKLE_ARCHIVE_BASE_URL
  CROWD_CAST_SPARKLE_PRIVATE_ED_KEY_FILE
EOF
}

require_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "Missing required command: $1" >&2
        exit 1
    fi
}

default_version() {
    sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1
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
        --sparkle-archive-base-url)
            SPARKLE_ARCHIVE_BASE_URL="$2"
            shift 2
            ;;
        --sparkle-private-ed-key-file)
            SPARKLE_PRIVATE_ED_KEY_FILE="$2"
            shift 2
            ;;
        --sparkle-release-notes-url-prefix)
            SPARKLE_RELEASE_NOTES_URL_PREFIX="$2"
            shift 2
            ;;
        --sparkle-full-release-notes-url)
            SPARKLE_FULL_RELEASE_NOTES_URL="$2"
            shift 2
            ;;
        --sparkle-product-link)
            SPARKLE_PRODUCT_LINK="$2"
            shift 2
            ;;
        --sparkle-channel)
            SPARKLE_CHANNEL="$2"
            shift 2
            ;;
        --sparkle-phased-rollout-interval)
            SPARKLE_PHASED_ROLLOUT_INTERVAL="$2"
            shift 2
            ;;
        --sparkle-critical-update-version)
            SPARKLE_CRITICAL_UPDATE_VERSION="$2"
            shift 2
            ;;
        --debug)
            BUILD_TYPE="debug"
            TARGET_DIR="target/debug"
            APP_PATH="${TARGET_DIR}/${APP_NAME}.app"
            DMG_PATH="${TARGET_DIR}/${APP_NAME}.dmg"
            SPARKLE_ARCHIVE_DIR="${TARGET_DIR}/sparkle"
            shift
            ;;
        --notarize)
            NOTARIZE=1
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
require_cmd create-dmg
require_cmd ditto

if [[ -z "$SIGN_IDENTITY" ]]; then
    echo "Missing signing identity. Pass --identity or set CROWD_CAST_MACOS_SIGN_IDENTITY." >&2
    exit 1
fi

if [[ -z "$API_GATEWAY_URL" ]]; then
    echo "Missing API gateway URL. Pass --api-gateway-url or set CROWD_CAST_API_GATEWAY_URL." >&2
    exit 1
fi

APP_VERSION="${APP_VERSION:-$(default_version)}"
BUILD_NUMBER="${BUILD_NUMBER:-$(date -u +%Y%m%d%H%M%S)}"

if [[ -n "$SPARKLE_ARCHIVE_BASE_URL" || -n "$SPARKLE_PRIVATE_ED_KEY_FILE" || -n "$SPARKLE_FEED_URL" || -n "$SPARKLE_PUBLIC_ED_KEY" ]]; then
    GENERATE_APPCAST=1
fi

if [[ "$GENERATE_APPCAST" -eq 1 ]]; then
    if [[ -z "$SPARKLE_FEED_URL" ]]; then
        echo "Missing Sparkle feed URL. Pass --feed-url or set CROWD_CAST_SPARKLE_FEED_URL." >&2
        exit 1
    fi
    if [[ -z "$SPARKLE_PUBLIC_ED_KEY" ]]; then
        echo "Missing Sparkle public EdDSA key. Pass --sparkle-public-ed-key or set CROWD_CAST_SPARKLE_PUBLIC_ED_KEY." >&2
        exit 1
    fi
    if [[ -z "$SPARKLE_ARCHIVE_BASE_URL" ]]; then
        echo "Missing Sparkle archive base URL. Pass --sparkle-archive-base-url or set CROWD_CAST_SPARKLE_ARCHIVE_BASE_URL." >&2
        exit 1
    fi
    if [[ -z "$SPARKLE_PRIVATE_ED_KEY_FILE" ]]; then
        echo "Missing Sparkle private EdDSA key file. Pass --sparkle-private-ed-key-file or set CROWD_CAST_SPARKLE_PRIVATE_ED_KEY_FILE." >&2
        exit 1
    fi
    if [[ "$NOTARIZE" -eq 0 ]]; then
        echo "Warning: generating Sparkle artifacts without notarization. End-user updates should normally be notarized." >&2
    fi
fi

echo "Step 1/5: Build and sign app bundle..."
if [[ "$BUILD_TYPE" == "debug" ]]; then
    CROWD_CAST_API_GATEWAY_URL="$API_GATEWAY_URL" scripts/bundle-macos.sh \
        --debug \
        --identity "$SIGN_IDENTITY" \
        --version "$APP_VERSION" \
        --build-number "$BUILD_NUMBER" \
        --feed-url "$SPARKLE_FEED_URL" \
        --sparkle-public-ed-key "$SPARKLE_PUBLIC_ED_KEY" \
        --no-verify
else
    CROWD_CAST_API_GATEWAY_URL="$API_GATEWAY_URL" scripts/bundle-macos.sh \
        --identity "$SIGN_IDENTITY" \
        --version "$APP_VERSION" \
        --build-number "$BUILD_NUMBER" \
        --feed-url "$SPARKLE_FEED_URL" \
        --sparkle-public-ed-key "$SPARKLE_PUBLIC_ED_KEY" \
        --no-verify
fi

if [[ ! -d "$APP_PATH" ]]; then
    echo "Expected app bundle not found: $APP_PATH" >&2
    exit 1
fi

echo "Step 2/5: Create drag-to-Applications DMG..."
rm -f "$DMG_PATH"

CREATE_DMG_ARGS=(
    --volname "$APP_NAME"
    --window-pos 200 120
    --window-size 660 400
    --icon-size 128
    --text-size 13
    --icon "${APP_NAME}.app" 180 190
    --hide-extension "${APP_NAME}.app"
    --app-drop-link 480 190
    --no-internet-enable
    --format UDZO
)

if [[ -f "$DMG_BACKGROUND" ]]; then
    CREATE_DMG_ARGS+=(--background "$DMG_BACKGROUND")
fi

set +e
create-dmg "${CREATE_DMG_ARGS[@]}" "$DMG_PATH" "$APP_PATH"
CREATE_DMG_EXIT=$?
set -e

if [[ $CREATE_DMG_EXIT -ne 0 && $CREATE_DMG_EXIT -ne 2 ]]; then
    echo "create-dmg failed with exit code $CREATE_DMG_EXIT" >&2
    exit 1
fi

echo "Step 3/5: Sign DMG..."
codesign --force --timestamp --sign "$SIGN_IDENTITY" "$DMG_PATH"

if [[ "$NOTARIZE" -eq 1 ]]; then
    require_cmd xcrun
    echo "Step 4/5: Notarize DMG and staple artifacts..."
    xcrun notarytool submit "$DMG_PATH" --keychain-profile "$NOTARY_PROFILE" --wait
    xcrun stapler staple "$APP_PATH"
    xcrun stapler staple "$DMG_PATH"
else
    echo "Step 4/5: Skipped notarization/stapling (use --notarize to enable)."
fi

if [[ "$GENERATE_APPCAST" -eq 1 ]]; then
    echo "Step 4.5/5: Build Sparkle archive and appcast..."
    mkdir -p "$SPARKLE_ARCHIVE_DIR"
    SPARKLE_ARCHIVE_PATH="${SPARKLE_ARCHIVE_DIR}/${APP_NAME}-${APP_VERSION}+${BUILD_NUMBER}.zip"
    rm -f "$SPARKLE_ARCHIVE_PATH"
    ditto -c -k --sequesterRsrc --keepParent "$APP_PATH" "$SPARKLE_ARCHIVE_PATH"

    GENERATE_ARGS=(
        --archives-dir "$SPARKLE_ARCHIVE_DIR"
        --download-url-prefix "$SPARKLE_ARCHIVE_BASE_URL"
        --ed-key-file "$SPARKLE_PRIVATE_ED_KEY_FILE"
    )

    if [[ -n "$SPARKLE_RELEASE_NOTES_URL_PREFIX" ]]; then
        GENERATE_ARGS+=(--release-notes-url-prefix "$SPARKLE_RELEASE_NOTES_URL_PREFIX")
    fi
    if [[ -n "$SPARKLE_FULL_RELEASE_NOTES_URL" ]]; then
        GENERATE_ARGS+=(--full-release-notes-url "$SPARKLE_FULL_RELEASE_NOTES_URL")
    fi
    if [[ -n "$SPARKLE_PRODUCT_LINK" ]]; then
        GENERATE_ARGS+=(--link "$SPARKLE_PRODUCT_LINK")
    fi
    if [[ -n "$SPARKLE_CHANNEL" ]]; then
        GENERATE_ARGS+=(--channel "$SPARKLE_CHANNEL")
    fi
    if [[ -n "$SPARKLE_PHASED_ROLLOUT_INTERVAL" ]]; then
        GENERATE_ARGS+=(--phased-rollout-interval "$SPARKLE_PHASED_ROLLOUT_INTERVAL")
    fi
    if [[ -n "$SPARKLE_CRITICAL_UPDATE_VERSION" ]]; then
        GENERATE_ARGS+=(--critical-update-version "$SPARKLE_CRITICAL_UPDATE_VERSION")
    fi

    scripts/generate-appcast.sh "${GENERATE_ARGS[@]}"
fi

echo "Step 5/5: Verification gates..."
codesign --verify --strict --deep --verbose=2 "$APP_PATH"
if [[ "$NOTARIZE" -eq 1 ]]; then
    spctl --assess --type execute --verbose "$APP_PATH"
    spctl -a -t open --context context:primary-signature -vvv "$DMG_PATH"
else
    echo "Skipping Gatekeeper assessment because notarization is disabled."
fi

echo
echo "Release artifacts:"
echo "  App: $APP_PATH"
echo "  DMG: $DMG_PATH"
if [[ -n "$SPARKLE_ARCHIVE_PATH" ]]; then
    echo "  Sparkle ZIP: $SPARKLE_ARCHIVE_PATH"
    echo "  Sparkle appcast: ${SPARKLE_ARCHIVE_DIR}/appcast.xml"
fi
