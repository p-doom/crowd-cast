#!/bin/bash
# Build, sign, notarize, and publish a macOS release.
#
# Wraps release-macos.sh for the build pipeline, then:
#   1. Creates a GitHub Release with the signed ZIP and DMG
#   2. Uploads the Sparkle appcast.xml to S3 (publicly readable)
#
# Prerequisites:
#   - gh auth login   (GitHub CLI)
#   - aws login       (AWS CLI)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_ROOT"

APP_NAME="CrowdCast"

# --- Publish-specific args (not passed to release-macos.sh) ---
GITHUB_REPO="${CROWD_CAST_GITHUB_REPO:-}"
S3_BUCKET="${CROWD_CAST_S3_BUCKET:-}"
S3_APPCAST_KEY="${CROWD_CAST_S3_APPCAST_KEY:-appcast.xml}"
DRY_RUN=0

# --- Args we need to extract but also pass through ---
BUILD_NUMBER=""

# --- Remaining args forwarded to release-macos.sh ---
RELEASE_ARGS=()

usage() {
    cat <<EOF
Usage: scripts/build-and-publish-macos.sh [options]

Wraps release-macos.sh and publishes to GitHub Releases + S3.

Publish options:
  --github-repo <owner/repo>   GitHub repository (e.g. p-doom/crowd-cast)
  --s3-bucket <bucket>         S3 bucket for appcast.xml
  --s3-appcast-key <key>       S3 object key for appcast (default: appcast.xml)
  --dry-run                    Print publish steps without executing them

All other options are forwarded to release-macos.sh (run with -h to see them).
At minimum you need: --build-number, --identity.
Version is always read from Cargo.toml (single source of truth).

Environment fallbacks:
  CROWD_CAST_GITHUB_REPO
  CROWD_CAST_S3_BUCKET
  CROWD_CAST_S3_APPCAST_KEY
EOF
}

# --- Parse args ---
while [[ $# -gt 0 ]]; do
    case "$1" in
        --github-repo)
            GITHUB_REPO="$2"
            shift 2
            ;;
        --s3-bucket)
            S3_BUCKET="$2"
            shift 2
            ;;
        --s3-appcast-key)
            S3_APPCAST_KEY="$2"
            shift 2
            ;;
        --dry-run)
            DRY_RUN=1
            shift
            ;;
        --version)
            echo "Warning: --version is read from Cargo.toml, ignoring." >&2
            shift 2
            ;;
        --build-number)
            BUILD_NUMBER="$2"
            RELEASE_ARGS+=("$1" "$2")
            shift 2
            ;;
        --feed-url)
            echo "Warning: --feed-url is computed by this script, ignoring." >&2
            shift 2
            ;;
        --sparkle-archive-base-url)
            echo "Warning: --sparkle-archive-base-url is computed by this script, ignoring." >&2
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            RELEASE_ARGS+=("$1")
            shift
            ;;
    esac
done

# --- Read version from Cargo.toml (single source of truth) ---
APP_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"
if [[ -z "$APP_VERSION" ]]; then
    echo "Could not read version from Cargo.toml." >&2
    exit 1
fi

# --- Validate required args ---
if [[ -z "$GITHUB_REPO" ]]; then
    echo "Missing GitHub repo. Pass --github-repo or set CROWD_CAST_GITHUB_REPO." >&2
    exit 1
fi
if [[ -z "$S3_BUCKET" ]]; then
    echo "Missing S3 bucket. Pass --s3-bucket or set CROWD_CAST_S3_BUCKET." >&2
    exit 1
fi
if [[ -z "$BUILD_NUMBER" ]]; then
    echo "Missing --build-number." >&2
    exit 1
fi

# --- Derived values ---
RELEASE_TAG="v${APP_VERSION}-${BUILD_NUMBER}"
FEED_URL="https://${S3_BUCKET}.s3.amazonaws.com/${S3_APPCAST_KEY}"
GITHUB_DOWNLOAD_PREFIX="https://github.com/${GITHUB_REPO}/releases/download/${RELEASE_TAG}/"
SPARKLE_ARCHIVE_DIR="target/release/sparkle"
SPARKLE_ZIP="${APP_NAME}-${APP_VERSION}+${BUILD_NUMBER}.zip"
DMG_PATH="target/release/${APP_NAME}.dmg"

# --- Preflight checks ---
echo "Preflight: checking credentials..."
gh auth status >/dev/null 2>&1 || { echo "Not logged into GitHub CLI. Run: gh auth login" >&2; exit 1; }
aws sts get-caller-identity >/dev/null 2>&1 || { echo "AWS session expired. Run: aws login" >&2; exit 1; }

if gh release view "$RELEASE_TAG" --repo "$GITHUB_REPO" >/dev/null 2>&1; then
    echo "Release $RELEASE_TAG already exists on GitHub." >&2
    exit 1
fi

echo "Will publish as: $RELEASE_TAG"
echo "  GitHub Release: https://github.com/${GITHUB_REPO}/releases/tag/${RELEASE_TAG}"
echo "  Appcast feed:   ${FEED_URL}"
echo

# --- Build ---
echo "=== Building release ==="
scripts/release-macos.sh \
    "${RELEASE_ARGS[@]}" \
    --feed-url "$FEED_URL" \
    --sparkle-archive-base-url "$GITHUB_DOWNLOAD_PREFIX"

# --- Verify artifacts exist ---
if [[ ! -f "$SPARKLE_ARCHIVE_DIR/$SPARKLE_ZIP" ]]; then
    echo "Expected Sparkle archive not found: $SPARKLE_ARCHIVE_DIR/$SPARKLE_ZIP" >&2
    exit 1
fi
if [[ ! -f "$DMG_PATH" ]]; then
    echo "Expected DMG not found: $DMG_PATH" >&2
    exit 1
fi
if [[ ! -f "$SPARKLE_ARCHIVE_DIR/appcast.xml" ]]; then
    echo "Expected appcast.xml not found: $SPARKLE_ARCHIVE_DIR/appcast.xml" >&2
    exit 1
fi

# --- Publish to GitHub Releases ---
echo
echo "=== Publishing to GitHub Releases ==="
if [[ "$DRY_RUN" -eq 1 ]]; then
    echo "[dry-run] gh release create $RELEASE_TAG"
    echo "[dry-run]   $SPARKLE_ARCHIVE_DIR/$SPARKLE_ZIP"
    echo "[dry-run]   $DMG_PATH"
else
    gh release create "$RELEASE_TAG" \
        "$SPARKLE_ARCHIVE_DIR/$SPARKLE_ZIP" \
        "$DMG_PATH" \
        --repo "$GITHUB_REPO" \
        --title "$RELEASE_TAG" \
        --generate-notes
fi

# --- Upload appcast.xml to S3 (publicly readable) ---
echo
echo "=== Uploading appcast.xml to S3 ==="
if [[ "$DRY_RUN" -eq 1 ]]; then
    echo "[dry-run] aws s3 cp appcast.xml -> s3://${S3_BUCKET}/${S3_APPCAST_KEY}"
else
    aws s3 cp "$SPARKLE_ARCHIVE_DIR/appcast.xml" \
        "s3://${S3_BUCKET}/${S3_APPCAST_KEY}"
fi

echo
echo "Published $RELEASE_TAG"
echo "  GitHub: https://github.com/${GITHUB_REPO}/releases/tag/${RELEASE_TAG}"
echo "  Appcast: ${FEED_URL}"
