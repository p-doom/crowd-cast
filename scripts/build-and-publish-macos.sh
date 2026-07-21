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
CHANNEL="${CROWD_CAST_CHANNEL:-dev}"
DRY_RUN=0
ALLOW_MISMATCH=0

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
  --allow-version-mismatch     Release even if Windows is on a different marketing version

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
        --channel)
            CHANNEL="$2"
            shift 2
            ;;
        --dry-run)
            DRY_RUN=1
            shift
            ;;
        --allow-version-mismatch)
            ALLOW_MISMATCH=1
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

# --- Channel config ---
if [[ "$CHANNEL" == "prod" ]]; then
    S3_APPCAST_KEY="appcast.xml"
    GH_PRERELEASE=""
elif [[ "$CHANNEL" == "dev" ]]; then
    S3_APPCAST_KEY="appcast-dev.xml"
    GH_PRERELEASE="--prerelease"
else
    echo "Unknown channel: $CHANNEL (use 'dev' or 'prod')" >&2
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

# Version guard: refuse to release macOS at a marketing version that differs from
# the latest Windows release, so the carried-forward exe and the download buttons
# never advertise two different marketing versions. Override with --allow-version-mismatch.
WIN_MKT=""
while IFS= read -r tag; do
    if [[ "$tag" =~ ^win-v([0-9.]+)[+] ]]; then WIN_MKT="${BASH_REMATCH[1]}"; break; fi
done < <(gh release list --repo "$GITHUB_REPO" --limit 40 --json tagName,createdAt --jq 'sort_by(.createdAt) | reverse | .[].tagName')
if [[ -n "$WIN_MKT" && "$WIN_MKT" != "$APP_VERSION" ]]; then
    if [[ "$ALLOW_MISMATCH" -eq 1 ]]; then
        echo "Warning: releasing macOS $APP_VERSION while the latest Windows release is $WIN_MKT (proceeding, --allow-version-mismatch)." >&2
    else
        echo "Version mismatch: releasing macOS $APP_VERSION while the latest Windows release is $WIN_MKT." >&2
        echo "Release Windows $APP_VERSION first, or pass --allow-version-mismatch." >&2
        exit 1
    fi
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

# --- Carry the current Windows installer forward ---
# The download buttons use /releases/latest/download/..., and this macOS release
# becomes "Latest" when it is the most recent, so it must also carry the current
# crowd-cast-setup.exe or the Windows button 404s. We copy it from the most recent
# release that has one. The Windows WinSparkle appcast still points at its own
# immutable asset, so auto-update is unaffected; this is purely the human download.
CARRY_DIR="target/release/carry-forward"
WIN_EXE="$CARRY_DIR/crowd-cast-setup.exe"
rm -rf "$CARRY_DIR" && mkdir -p "$CARRY_DIR"
WIN_TAG=""
while IFS= read -r tag; do
    [[ -z "$tag" ]] && continue
    names="$(gh release view "$tag" --repo "$GITHUB_REPO" --json assets --jq '.assets[].name' 2>/dev/null || true)"
    if grep -qx 'crowd-cast-setup.exe' <<<"$names"; then WIN_TAG="$tag"; break; fi
done < <(gh release list --repo "$GITHUB_REPO" --limit 40 --json tagName,createdAt --jq 'sort_by(.createdAt) | reverse | .[].tagName')

# Assets always include the Sparkle zip + dmg; append the carried exe if found.
ASSETS=("$SPARKLE_ARCHIVE_DIR/$SPARKLE_ZIP" "$DMG_PATH")
if [[ -n "$WIN_TAG" ]]; then
    if [[ "$DRY_RUN" -eq 1 ]]; then
        echo "[dry-run] would carry forward crowd-cast-setup.exe from $WIN_TAG"
    else
        gh release download "$WIN_TAG" --repo "$GITHUB_REPO" \
            --pattern 'crowd-cast-setup.exe' --dir "$CARRY_DIR" --clobber
        echo "Carried forward crowd-cast-setup.exe from $WIN_TAG"
    fi
    ASSETS+=("$WIN_EXE")
else
    echo "No Windows crowd-cast-setup.exe found yet; publishing macOS-only (win button stays 404 until Windows releases)."
fi

# --- Carry the current Linux assets forward ---
# Same reason as the Windows exe above: this release becomes "Latest", and the
# website's Linux install button (and page logo) resolve via
# /releases/latest/download/, so crowd-cast-agent-x86_64, install-linux.sh, and
# logo.png must ride along or those URLs 404. (Observed live: v1.0.7-1097
# 404'd the Linux button for the minutes until linux-v1.0.7+11 took "Latest"
# back — a macOS-last release would have left it 404 indefinitely.) The Linux
# release's own asset URLs are immutable, so its auto-update/install flows are
# unaffected; this is purely the human download path.
LINUX_CARRY_ASSETS=(crowd-cast-agent-x86_64 install-linux.sh logo.png)
LINUX_TAG=""
LINUX_TAG_NAMES=""
while IFS= read -r tag; do
    [[ -z "$tag" ]] && continue
    names="$(gh release view "$tag" --repo "$GITHUB_REPO" --json assets --jq '.assets[].name' 2>/dev/null || true)"
    if grep -qx 'crowd-cast-agent-x86_64' <<<"$names"; then LINUX_TAG="$tag"; LINUX_TAG_NAMES="$names"; break; fi
done < <(gh release list --repo "$GITHUB_REPO" --limit 40 --json tagName,createdAt --jq 'sort_by(.createdAt) | reverse | .[].tagName')

if [[ -n "$LINUX_TAG" ]]; then
    for asset in "${LINUX_CARRY_ASSETS[@]}"; do
        if ! grep -qx "$asset" <<<"$LINUX_TAG_NAMES"; then
            echo "Warning: $LINUX_TAG has no $asset; skipping that carry-forward." >&2
            continue
        fi
        if [[ "$DRY_RUN" -eq 1 ]]; then
            echo "[dry-run] would carry forward $asset from $LINUX_TAG"
        else
            gh release download "$LINUX_TAG" --repo "$GITHUB_REPO" \
                --pattern "$asset" --dir "$CARRY_DIR" --clobber
            echo "Carried forward $asset from $LINUX_TAG"
        fi
        ASSETS+=("$CARRY_DIR/$asset")
    done
else
    echo "No Linux crowd-cast-agent-x86_64 found yet; publishing without Linux assets (linux button stays 404 until Linux releases)."
fi

# --- Publish to GitHub Releases ---
echo
echo "=== Publishing to GitHub Releases ==="
if [[ "$DRY_RUN" -eq 1 ]]; then
    echo "[dry-run] gh release create $RELEASE_TAG"
    for a in "${ASSETS[@]}"; do echo "[dry-run]   $a"; done
else
    gh release create "$RELEASE_TAG" \
        "${ASSETS[@]}" \
        --repo "$GITHUB_REPO" \
        --title "$RELEASE_TAG" \
        --generate-notes \
        $GH_PRERELEASE
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
