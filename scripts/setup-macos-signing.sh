#!/bin/bash
# First-time local setup for macOS Developer ID signing and notarization.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_ROOT"

P12_PATH=""
P12_PASSWORD=""
KEYCHAIN_PATH="${CROWD_CAST_KEYCHAIN_PATH:-$HOME/Library/Keychains/login.keychain-db}"
KEYCHAIN_PASSWORD=""
NOTARY_PROFILE="${CROWD_CAST_NOTARY_PROFILE:-crowdcast-notary}"
APPLE_ID="${CROWD_CAST_APPLE_ID:-}"
TEAM_ID="${CROWD_CAST_TEAM_ID:-}"
APP_SPECIFIC_PASSWORD="${CROWD_CAST_APPLE_APP_SPECIFIC_PASSWORD:-}"
SETUP_NOTARY=1
DEV_ID_G2_CA_URL="https://www.apple.com/certificateauthority/DeveloperIDG2CA.cer"

usage() {
    cat <<EOF
Usage: scripts/setup-macos-signing.sh [options]

Options:
  --p12 <path>                Path to Developer ID .p12 certificate (required)
  --p12-password <password>   .p12 import password
  --keychain <path>           Keychain path (default: login.keychain-db)
  --keychain-password <pass>  Keychain password
  --notary-profile <name>     notarytool profile (default: crowdcast-notary)
  --apple-id <id>             Apple ID email for notarization
  --team-id <id>              Apple Team ID
  --app-password <password>   Apple app-specific password
  --skip-notary               Skip notarytool profile setup
  -h, --help                  Show this help

Environment fallbacks:
  CROWD_CAST_KEYCHAIN_PATH
  CROWD_CAST_NOTARY_PROFILE
  CROWD_CAST_APPLE_ID
  CROWD_CAST_TEAM_ID
  CROWD_CAST_APPLE_APP_SPECIFIC_PASSWORD
EOF
}

require_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "Missing required command: $1" >&2
        exit 1
    fi
}

identity_matching_count() {
    local count
    count="$(
        security find-identity -p codesigning "$KEYCHAIN_PATH" 2>/dev/null \
            | awk '/identities found/{print $1; exit}'
    )"
    if [[ "$count" =~ ^[0-9]+$ ]]; then
        echo "$count"
    else
        echo "0"
    fi
}

identity_valid_count() {
    local count
    count="$(
        security find-identity -v -p codesigning "$KEYCHAIN_PATH" 2>/dev/null \
            | awk '/valid identities found/{print $1; exit}'
    )"
    if [[ "$count" =~ ^[0-9]+$ ]]; then
        echo "$count"
    else
        echo "0"
    fi
}

auto_fix_developer_id_chain_if_needed() {
    local matching valid tmp_cert

    matching="$(identity_matching_count)"
    valid="$(identity_valid_count)"

    if [[ "$matching" -gt 0 && "$valid" -eq 0 ]]; then
        echo
        echo "Detected matching code signing identities but none are currently valid."
        echo "Attempting to import Developer ID G2 intermediate certificate..."

        if ! command -v curl >/dev/null 2>&1; then
            echo "Skipping auto-fix because 'curl' is not installed."
            return
        fi

        tmp_cert="$(mktemp /tmp/developeridg2ca.XXXXXX.cer)"
        if curl -fsSL "$DEV_ID_G2_CA_URL" -o "$tmp_cert"; then
            if security add-certificates -k "$KEYCHAIN_PATH" "$tmp_cert" >/dev/null 2>&1; then
                echo "Imported Developer ID G2 intermediate certificate."
            else
                echo "Developer ID G2 intermediate cert could not be added (already present or keychain refused import)."
            fi
        else
            echo "Failed to download Developer ID G2 intermediate cert from:"
            echo "  $DEV_ID_G2_CA_URL"
        fi
        rm -f "$tmp_cert"
    fi
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --p12)
            P12_PATH="$2"
            shift 2
            ;;
        --p12-password)
            P12_PASSWORD="$2"
            shift 2
            ;;
        --keychain)
            KEYCHAIN_PATH="$2"
            shift 2
            ;;
        --keychain-password)
            KEYCHAIN_PASSWORD="$2"
            shift 2
            ;;
        --notary-profile)
            NOTARY_PROFILE="$2"
            shift 2
            ;;
        --apple-id)
            APPLE_ID="$2"
            shift 2
            ;;
        --team-id)
            TEAM_ID="$2"
            shift 2
            ;;
        --app-password)
            APP_SPECIFIC_PASSWORD="$2"
            shift 2
            ;;
        --skip-notary)
            SETUP_NOTARY=0
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

require_cmd security
require_cmd codesign

if [[ -z "$P12_PATH" ]]; then
    echo "Missing required --p12 <path>" >&2
    usage
    exit 1
fi

if [[ ! -f "$P12_PATH" ]]; then
    echo "p12 file not found: $P12_PATH" >&2
    exit 1
fi

if [[ -z "$P12_PASSWORD" ]]; then
    read -r -s -p "Enter .p12 password: " P12_PASSWORD
    echo
fi

if [[ -z "$KEYCHAIN_PASSWORD" ]]; then
    read -r -s -p "Enter keychain password for $KEYCHAIN_PATH: " KEYCHAIN_PASSWORD
    echo
fi

if [[ ! -f "$KEYCHAIN_PATH" ]]; then
    echo "Keychain not found: $KEYCHAIN_PATH" >&2
    exit 1
fi

echo "Importing signing certificate into keychain..."
security import "$P12_PATH" \
    -k "$KEYCHAIN_PATH" \
    -P "$P12_PASSWORD" \
    -T /usr/bin/codesign \
    -T /usr/bin/security \
    -T /usr/bin/xcrun

echo "Configuring keychain partition list for non-interactive codesign..."
security set-key-partition-list \
    -S apple-tool:,apple: \
    -s \
    -k "$KEYCHAIN_PASSWORD" \
    "$KEYCHAIN_PATH"

echo
echo "Available code signing identities:"
security find-identity -p codesigning "$KEYCHAIN_PATH"

auto_fix_developer_id_chain_if_needed

echo
echo "Post-setup code signing identities:"
security find-identity -p codesigning "$KEYCHAIN_PATH"

if [[ "$SETUP_NOTARY" -eq 1 ]]; then
    require_cmd xcrun

    if [[ -z "$APPLE_ID" ]]; then
        read -r -p "Apple ID email for notarization: " APPLE_ID
    fi
    if [[ -z "$TEAM_ID" ]]; then
        read -r -p "Apple Team ID: " TEAM_ID
    fi
    if [[ -z "$APP_SPECIFIC_PASSWORD" ]]; then
        read -r -s -p "Apple app-specific password: " APP_SPECIFIC_PASSWORD
        echo
    fi

    echo "Storing notarytool credentials profile: $NOTARY_PROFILE"
    xcrun notarytool store-credentials "$NOTARY_PROFILE" \
        --apple-id "$APPLE_ID" \
        --team-id "$TEAM_ID" \
        --password "$APP_SPECIFIC_PASSWORD"
fi

echo
echo "Setup completed."
echo "Next:"
echo "  1) Pick your identity from 'security find-identity' output."
echo "  2) Run scripts/release-macos.sh with that identity."
