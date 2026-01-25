#!/bin/bash
# Build macOS DMG for crowd-cast

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BUILD_DIR="$PROJECT_ROOT/build/macos"
APP_NAME="crowd-cast"
APP_BUNDLE="$BUILD_DIR/$APP_NAME.app"

echo "Building crowd-cast for macOS..."

# Clean and create build directory
rm -rf "$BUILD_DIR"
mkdir -p "$APP_BUNDLE/Contents/MacOS"
mkdir -p "$APP_BUNDLE/Contents/Resources"
mkdir -p "$APP_BUNDLE/Contents/Resources/plugins"
mkdir -p "$APP_BUNDLE/Contents/Resources/data/locale"

# Build the Rust agent for both architectures
echo "Building agent (universal binary)..."
cd "$PROJECT_ROOT/agent"

# Build for both architectures if possible
if rustup target list --installed | grep -q "aarch64-apple-darwin"; then
    cargo build --release --target aarch64-apple-darwin
    AARCH64_BIN="$PROJECT_ROOT/agent/target/aarch64-apple-darwin/release/crowd-cast-agent"
else
    AARCH64_BIN=""
fi

if rustup target list --installed | grep -q "x86_64-apple-darwin"; then
    cargo build --release --target x86_64-apple-darwin
    X86_64_BIN="$PROJECT_ROOT/agent/target/x86_64-apple-darwin/release/crowd-cast-agent"
else
    X86_64_BIN=""
fi

# Create universal binary or use single architecture
if [ -n "$AARCH64_BIN" ] && [ -n "$X86_64_BIN" ]; then
    echo "Creating universal binary..."
    lipo -create "$AARCH64_BIN" "$X86_64_BIN" -output "$APP_BUNDLE/Contents/MacOS/crowd-cast-agent"
elif [ -n "$AARCH64_BIN" ]; then
    cp "$AARCH64_BIN" "$APP_BUNDLE/Contents/MacOS/crowd-cast-agent"
elif [ -n "$X86_64_BIN" ]; then
    cp "$X86_64_BIN" "$APP_BUNDLE/Contents/MacOS/crowd-cast-agent"
else
    # Fallback to default target
    cargo build --release
    cp "$PROJECT_ROOT/agent/target/release/crowd-cast-agent" "$APP_BUNDLE/Contents/MacOS/"
fi

# Copy Info.plist
cp "$SCRIPT_DIR/Info.plist" "$APP_BUNDLE/Contents/"

# Create PkgInfo
echo "APPL????" > "$APP_BUNDLE/Contents/PkgInfo"

# Copy OBS plugin (if built)
if [ -f "$PROJECT_ROOT/obs-crowd-cast-plugin/build/obs-crowd-cast.so" ]; then
    cp "$PROJECT_ROOT/obs-crowd-cast-plugin/build/obs-crowd-cast.so" "$APP_BUNDLE/Contents/Resources/plugins/"
    cp "$PROJECT_ROOT/obs-crowd-cast-plugin/data/locale/en-US.ini" "$APP_BUNDLE/Contents/Resources/data/locale/"
fi

# Copy icon (if exists)
if [ -f "$PROJECT_ROOT/resources/icons/AppIcon.icns" ]; then
    cp "$PROJECT_ROOT/resources/icons/AppIcon.icns" "$APP_BUNDLE/Contents/Resources/"
fi

# Create a wrapper script that runs setup on first launch
cat > "$APP_BUNDLE/Contents/MacOS/crowd-cast" << 'EOF'
#!/bin/bash
DIR="$(cd "$(dirname "$0")" && pwd)"
exec "$DIR/crowd-cast-agent" "$@"
EOF
chmod +x "$APP_BUNDLE/Contents/MacOS/crowd-cast"

# Code sign (ad-hoc for local builds)
echo "Code signing..."
codesign --force --deep --sign - "$APP_BUNDLE" 2>/dev/null || echo "Warning: Code signing failed (may need Developer ID)"

# Create DMG
echo "Creating DMG..."
DMG_NAME="crowd-cast-$(sw_vers -productVersion | cut -d. -f1).dmg"

# Create temporary DMG directory
DMG_DIR="$BUILD_DIR/dmg"
mkdir -p "$DMG_DIR"
cp -R "$APP_BUNDLE" "$DMG_DIR/"

# Create symlink to Applications
ln -s /Applications "$DMG_DIR/Applications"

# Create the DMG
hdiutil create -volname "$APP_NAME" -srcfolder "$DMG_DIR" -ov -format UDZO "$BUILD_DIR/$DMG_NAME"

# Cleanup
rm -rf "$DMG_DIR"

echo "Done! DMG created at: $BUILD_DIR/$DMG_NAME"
