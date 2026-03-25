#!/bin/bash
set -euo pipefail

BUNDLE_ID="dev.crowd-cast.agent"

echo "Stopping running app/processes..."
pkill -f 'crowd-cast-agent' 2>/dev/null || true
osascript -e 'tell application "CrowdCast" to quit' 2>/dev/null || true

echo "Removing launch agent..."
launchctl bootout "gui/$(id -u)" "$HOME/Library/LaunchAgents/${BUNDLE_ID}.plist" 2>/dev/null || true
rm -f "$HOME/Library/LaunchAgents/${BUNDLE_ID}.plist"

echo "Removing installed app copies..."
rm -rf "/Applications/CrowdCast.app"
rm -rf "$HOME/Applications/CrowdCast.app"

echo "Removing app state..."
rm -rf "$HOME/Library/Application Support/${BUNDLE_ID}"
rm -rf "$HOME/Library/Caches/${BUNDLE_ID}"
rm -f "$HOME/Library/Preferences/${BUNDLE_ID}.plist"
rm -rf "$HOME/Library/Saved Application State/${BUNDLE_ID}.savedState"
rm -rf "$HOME/Library/HTTPStorages/${BUNDLE_ID}"
rm -rf "$HOME/Library/WebKit/${BUNDLE_ID}"
rm -rf "$HOME/Library/Logs/crowd-cast"

echo "Resetting macOS privacy permissions..."
# Reset individual services first to fully remove stale UI entries,
# then reset All as a catch-all. Without this, old entries can remain
# visible in System Settings in a broken state after reinstall.
for service in Accessibility ScreenCapture PostEvent ListenEvent Microphone Camera; do
    tccutil reset "$service" "$BUNDLE_ID" 2>/dev/null || true
done
tccutil reset All "$BUNDLE_ID" || true

echo
echo "Remaining app locations indexed by Spotlight:"
mdfind "kMDItemCFBundleIdentifier == '$BUNDLE_ID'" || true

echo
echo "Remaining on-disk artifacts:"
ls -ld "$HOME/Library/Application Support/${BUNDLE_ID}" 2>/dev/null || true
ls -ld "$HOME/Library/Caches/${BUNDLE_ID}" 2>/dev/null || true
ls -l "$HOME/Library/LaunchAgents/${BUNDLE_ID}.plist" 2>/dev/null || true
ls -ld "$HOME/Library/Logs/crowd-cast" 2>/dev/null || true

echo
echo "Reset complete."
echo "For the most realistic end-user test:"
echo "1. Download the notarized CrowdCast.dmg fresh in a browser."
echo "2. Open the downloaded DMG."
echo "3. Drag CrowdCast.app into /Applications."
echo "4. Launch it from /Applications."