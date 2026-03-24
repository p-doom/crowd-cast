#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

ARCHIVES_DIR=""
DOWNLOAD_URL_PREFIX="${CROWD_CAST_SPARKLE_ARCHIVE_BASE_URL:-}"
RELEASE_NOTES_URL_PREFIX="${CROWD_CAST_SPARKLE_RELEASE_NOTES_URL_PREFIX:-}"
FULL_RELEASE_NOTES_URL="${CROWD_CAST_SPARKLE_FULL_RELEASE_NOTES_URL:-}"
PRODUCT_LINK="${CROWD_CAST_SPARKLE_PRODUCT_LINK:-}"
PRIVATE_ED_KEY_FILE="${CROWD_CAST_SPARKLE_PRIVATE_ED_KEY_FILE:-}"
CHANNEL="${CROWD_CAST_SPARKLE_CHANNEL:-}"
PHASED_ROLLOUT_INTERVAL="${CROWD_CAST_SPARKLE_PHASED_ROLLOUT_INTERVAL:-}"
CRITICAL_UPDATE_VERSION="${CROWD_CAST_SPARKLE_CRITICAL_UPDATE_VERSION:-}"
OUTPUT_NAME="${CROWD_CAST_SPARKLE_APPCAST_NAME:-appcast.xml}"
EMBED_RELEASE_NOTES=0

usage() {
    cat <<EOF
Usage: scripts/generate-appcast.sh --archives-dir <dir> --download-url-prefix <url> [options]

Options:
  --archives-dir <dir>              Directory containing Sparkle update archives
  --download-url-prefix <url>       Base URL used to download update archives
  --ed-key-file <file>              Private EdDSA key file for Sparkle signing
  --release-notes-url-prefix <url>  Base URL for release notes sidecar files
  --full-release-notes-url <url>    Full release notes page URL
  --link <url>                      Product website link
  --channel <name>                  Sparkle channel for newly generated updates
  --phased-rollout-interval <secs>  Enable phased rollout for new updates
  --critical-update-version <ver>   Mark the update as critical for older versions
  --embed-release-notes             Always embed release notes in the appcast
  --output-name <name>              Appcast filename (default: appcast.xml)
  -h, --help                        Show this help

Environment fallbacks:
  CROWD_CAST_SPARKLE_ARCHIVE_BASE_URL
  CROWD_CAST_SPARKLE_RELEASE_NOTES_URL_PREFIX
  CROWD_CAST_SPARKLE_FULL_RELEASE_NOTES_URL
  CROWD_CAST_SPARKLE_PRODUCT_LINK
  CROWD_CAST_SPARKLE_PRIVATE_ED_KEY_FILE
  CROWD_CAST_SPARKLE_CHANNEL
  CROWD_CAST_SPARKLE_PHASED_ROLLOUT_INTERVAL
  CROWD_CAST_SPARKLE_CRITICAL_UPDATE_VERSION
  CROWD_CAST_SPARKLE_APPCAST_NAME
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --archives-dir)
            ARCHIVES_DIR="$2"
            shift 2
            ;;
        --download-url-prefix)
            DOWNLOAD_URL_PREFIX="$2"
            shift 2
            ;;
        --ed-key-file)
            PRIVATE_ED_KEY_FILE="$2"
            shift 2
            ;;
        --release-notes-url-prefix)
            RELEASE_NOTES_URL_PREFIX="$2"
            shift 2
            ;;
        --full-release-notes-url)
            FULL_RELEASE_NOTES_URL="$2"
            shift 2
            ;;
        --link)
            PRODUCT_LINK="$2"
            shift 2
            ;;
        --channel)
            CHANNEL="$2"
            shift 2
            ;;
        --phased-rollout-interval)
            PHASED_ROLLOUT_INTERVAL="$2"
            shift 2
            ;;
        --critical-update-version)
            CRITICAL_UPDATE_VERSION="$2"
            shift 2
            ;;
        --embed-release-notes)
            EMBED_RELEASE_NOTES=1
            shift
            ;;
        --output-name)
            OUTPUT_NAME="$2"
            shift 2
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

if [[ -z "$ARCHIVES_DIR" ]]; then
    echo "Missing --archives-dir" >&2
    exit 1
fi

if [[ -z "$DOWNLOAD_URL_PREFIX" ]]; then
    echo "Missing --download-url-prefix or CROWD_CAST_SPARKLE_ARCHIVE_BASE_URL" >&2
    exit 1
fi

if [[ -z "$PRIVATE_ED_KEY_FILE" ]]; then
    echo "Missing --ed-key-file or CROWD_CAST_SPARKLE_PRIVATE_ED_KEY_FILE" >&2
    exit 1
fi

if [[ ! -d "$ARCHIVES_DIR" ]]; then
    echo "Archives directory does not exist: $ARCHIVES_DIR" >&2
    exit 1
fi

if [[ ! -f "$PRIVATE_ED_KEY_FILE" ]]; then
    echo "Private EdDSA key file not found: $PRIVATE_ED_KEY_FILE" >&2
    exit 1
fi

"$PROJECT_ROOT/scripts/fetch-sparkle.sh" >/dev/null
SPARKLE_DIR="$("$PROJECT_ROOT/scripts/fetch-sparkle.sh" --print-dir)"
GENERATE_APPCAST="$SPARKLE_DIR/bin/generate_appcast"

if [[ ! -x "$GENERATE_APPCAST" ]]; then
    echo "Missing generate_appcast tool at $GENERATE_APPCAST" >&2
    exit 1
fi

ARGS=(
    --ed-key-file "$PRIVATE_ED_KEY_FILE"
    --download-url-prefix "$DOWNLOAD_URL_PREFIX"
    -o "$ARCHIVES_DIR/$OUTPUT_NAME"
)

if [[ -n "$RELEASE_NOTES_URL_PREFIX" ]]; then
    ARGS+=(--release-notes-url-prefix "$RELEASE_NOTES_URL_PREFIX")
fi

if [[ -n "$FULL_RELEASE_NOTES_URL" ]]; then
    ARGS+=(--full-release-notes-url "$FULL_RELEASE_NOTES_URL")
fi

if [[ -n "$PRODUCT_LINK" ]]; then
    ARGS+=(--link "$PRODUCT_LINK")
fi

if [[ -n "$CHANNEL" ]]; then
    ARGS+=(--channel "$CHANNEL")
fi

if [[ -n "$PHASED_ROLLOUT_INTERVAL" ]]; then
    ARGS+=(--phased-rollout-interval "$PHASED_ROLLOUT_INTERVAL")
fi

if [[ -n "$CRITICAL_UPDATE_VERSION" ]]; then
    ARGS+=(--critical-update-version "$CRITICAL_UPDATE_VERSION")
fi

if [[ "$EMBED_RELEASE_NOTES" -eq 1 ]]; then
    ARGS+=(--embed-release-notes)
fi

echo "Generating Sparkle appcast in $ARCHIVES_DIR..."
"$GENERATE_APPCAST" "${ARGS[@]}" "$ARCHIVES_DIR"

echo "Generated appcast:"
echo "  $ARCHIVES_DIR/$OUTPUT_NAME"
