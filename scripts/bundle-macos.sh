#!/bin/bash
# Bundle crowd-cast-agent as a macOS .app bundle with all OBS dependencies
set -e

APP_NAME="CrowdCast"
BUNDLE_ID="dev.crowd-cast.agent"
BINARY_NAME="crowd-cast-agent"

# Determine script directory and project root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

cd "$PROJECT_ROOT"

# Parse arguments
BUILD_TYPE="release"
if [[ "$1" == "--debug" ]]; then
    BUILD_TYPE="debug"
fi

echo "Building $BUILD_TYPE binary..."
if [[ "$BUILD_TYPE" == "release" ]]; then
    cargo build --release
else
    cargo build
fi

TARGET_DIR="target/${BUILD_TYPE}"

# Create .app structure
APP_DIR="${TARGET_DIR}/${APP_NAME}.app"
echo "Creating app bundle at: $APP_DIR"

rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS"
mkdir -p "$APP_DIR/Contents/Resources"
mkdir -p "$APP_DIR/Contents/Frameworks"

# Copy binary
cp "${TARGET_DIR}/${BINARY_NAME}" "$APP_DIR/Contents/MacOS/"

# Copy Info.plist
cp "resources/macos/Info.plist" "$APP_DIR/Contents/"

# Copy app icon
if [ -f "resources/macos/AppIcon.icns" ]; then
    cp "resources/macos/AppIcon.icns" "$APP_DIR/Contents/Resources/"
    echo "Copied app icon"
fi

# Copy tray icon to Resources
if [ -f "assets/logo.png" ]; then
    cp "assets/logo.png" "$APP_DIR/Contents/Resources/"
    echo "Copied tray icon"
fi

# Copy OBS frameworks
echo "Copying OBS frameworks..."
for framework in "${TARGET_DIR}"/*.framework; do
    if [ -d "$framework" ]; then
        framework_name=$(basename "$framework")
        echo "  Copying $framework_name"
        cp -R "$framework" "$APP_DIR/Contents/Frameworks/"
    fi
done

# Copy OBS dylibs
echo "Copying OBS dylibs..."
for dylib in "${TARGET_DIR}"/*.dylib; do
    if [ -f "$dylib" ]; then
        dylib_name=$(basename "$dylib")
        echo "  Copying $dylib_name"
        cp "$dylib" "$APP_DIR/Contents/Frameworks/"
    fi
done

# Copy OBS plugins
if [ -d "${TARGET_DIR}/obs-plugins" ]; then
    echo "Copying OBS plugins..."
    cp -R "${TARGET_DIR}/obs-plugins" "$APP_DIR/Contents/Resources/"
fi

# Copy OBS data directory
if [ -d "${TARGET_DIR}/data" ]; then
    echo "Copying OBS data..."
    cp -R "${TARGET_DIR}/data" "$APP_DIR/Contents/Resources/"
fi

# Update rpath in the binary to find frameworks in the bundle
echo "Updating binary rpaths..."
install_name_tool -add_rpath "@executable_path/../Frameworks" "$APP_DIR/Contents/MacOS/${BINARY_NAME}" 2>/dev/null || true

# Ad-hoc sign (required for UNUserNotificationCenter and other system features)
echo "Signing app bundle..."
codesign --force --deep --sign - "$APP_DIR"

echo ""
echo "Successfully created: $APP_DIR"
echo ""
echo "Bundle contents:"
du -sh "$APP_DIR/Contents/MacOS" 2>/dev/null || true
du -sh "$APP_DIR/Contents/Frameworks" 2>/dev/null || true
du -sh "$APP_DIR/Contents/Resources" 2>/dev/null || true
echo ""
echo "To run the app:"
echo "  open \"$APP_DIR\""
echo ""
echo "To install to Applications:"
echo "  cp -r \"$APP_DIR\" /Applications/"
