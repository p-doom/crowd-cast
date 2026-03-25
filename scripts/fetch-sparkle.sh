#!/bin/bash
set -euo pipefail

SPARKLE_VERSION="${CROWD_CAST_SPARKLE_VERSION:-2.8.1}"
SPARKLE_SHA256="${CROWD_CAST_SPARKLE_SHA256:-5cddb7695674ef7704268f38eccaee80e3accbf19e61c1689efff5b6116d85be}"
SPARKLE_URL="${CROWD_CAST_SPARKLE_URL:-https://github.com/sparkle-project/Sparkle/releases/download/${SPARKLE_VERSION}/Sparkle-${SPARKLE_VERSION}.tar.xz}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DEFAULT_DEST="${PROJECT_ROOT}/build/sparkle/${SPARKLE_VERSION}"
DEST_DIR="${CROWD_CAST_SPARKLE_DIR:-$DEFAULT_DEST}"
PRINT_DIR=0

usage() {
    cat <<EOF
Usage: scripts/fetch-sparkle.sh [options]

Options:
  --dest <dir>    Destination directory (default: build/sparkle/<version>)
  --print-dir     Print the resolved Sparkle directory and exit
  -h, --help      Show this help

Environment:
  CROWD_CAST_SPARKLE_VERSION
  CROWD_CAST_SPARKLE_SHA256
  CROWD_CAST_SPARKLE_URL
  CROWD_CAST_SPARKLE_DIR
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dest)
            DEST_DIR="$2"
            shift 2
            ;;
        --print-dir)
            PRINT_DIR=1
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

if [[ "$PRINT_DIR" -eq 1 ]]; then
    echo "$DEST_DIR"
    exit 0
fi

mkdir -p "$(dirname "$DEST_DIR")"

if [[ -d "$DEST_DIR/Sparkle.framework" && -x "$DEST_DIR/bin/generate_appcast" ]]; then
    echo "Sparkle already available at $DEST_DIR"
    exit 0
fi

TMP_DIR="$(mktemp -d)"
cleanup() {
    rm -rf "$TMP_DIR"
}
trap cleanup EXIT

ARCHIVE_PATH="$TMP_DIR/Sparkle-${SPARKLE_VERSION}.tar.xz"
EXTRACT_DIR="$TMP_DIR/extracted"

echo "Downloading Sparkle ${SPARKLE_VERSION}..."
curl -fsSL "$SPARKLE_URL" -o "$ARCHIVE_PATH"

ACTUAL_SHA256="$(shasum -a 256 "$ARCHIVE_PATH" | awk '{print $1}')"
if [[ "$ACTUAL_SHA256" != "$SPARKLE_SHA256" ]]; then
    echo "Sparkle checksum mismatch." >&2
    echo "Expected: $SPARKLE_SHA256" >&2
    echo "Actual:   $ACTUAL_SHA256" >&2
    exit 1
fi

mkdir -p "$EXTRACT_DIR"
tar -xf "$ARCHIVE_PATH" -C "$EXTRACT_DIR"

STAGING_DIR="${DEST_DIR}.tmp"
rm -rf "$STAGING_DIR"
mkdir -p "$STAGING_DIR"

cp -R "$EXTRACT_DIR/." "$STAGING_DIR/"

rm -rf "$DEST_DIR"
mv "$STAGING_DIR" "$DEST_DIR"

echo "Sparkle ${SPARKLE_VERSION} installed at $DEST_DIR"
