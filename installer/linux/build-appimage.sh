#!/bin/bash
# Build AppImage for CrowdCast

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BUILD_DIR="$PROJECT_ROOT/build/appimage"
APPDIR="$BUILD_DIR/CrowdCast.AppDir"

echo "Building CrowdCast AppImage..."

# Clean and create build directory
rm -rf "$BUILD_DIR"
mkdir -p "$APPDIR/usr/bin"
mkdir -p "$APPDIR/usr/lib/obs-plugins"
mkdir -p "$APPDIR/usr/share/applications"
mkdir -p "$APPDIR/usr/share/icons/hicolor/256x256/apps"
mkdir -p "$APPDIR/usr/share/obs/obs-plugins/obs-crowdcast/locale"

# Build the Rust agent
echo "Building agent..."
cd "$PROJECT_ROOT/agent"
cargo build --release

# Copy the agent binary
cp "$PROJECT_ROOT/agent/target/release/crowdcast-agent" "$APPDIR/usr/bin/"

# Copy the OBS plugin (if built)
if [ -f "$PROJECT_ROOT/obs-crowdcast-plugin/build/obs-crowdcast.so" ]; then
    cp "$PROJECT_ROOT/obs-crowdcast-plugin/build/obs-crowdcast.so" "$APPDIR/usr/lib/obs-plugins/"
    cp "$PROJECT_ROOT/obs-crowdcast-plugin/data/locale/en-US.ini" "$APPDIR/usr/share/obs/obs-plugins/obs-crowdcast/locale/"
fi

# Copy desktop file
cp "$SCRIPT_DIR/crowdcast.desktop" "$APPDIR/usr/share/applications/"
cp "$SCRIPT_DIR/crowdcast.desktop" "$APPDIR/"

# Copy icon (create placeholder if not exists)
if [ -f "$PROJECT_ROOT/resources/icons/crowdcast.png" ]; then
    cp "$PROJECT_ROOT/resources/icons/crowdcast.png" "$APPDIR/usr/share/icons/hicolor/256x256/apps/"
    cp "$PROJECT_ROOT/resources/icons/crowdcast.png" "$APPDIR/crowdcast.png"
else
    # Create a simple placeholder icon
    echo "Warning: No icon found, using placeholder"
    convert -size 256x256 xc:#4CAF50 -fill white -gravity center -pointsize 48 -annotate 0 "CC" "$APPDIR/crowdcast.png" 2>/dev/null || true
fi

# Create AppRun script
cat > "$APPDIR/AppRun" << 'EOF'
#!/bin/bash
SELF=$(readlink -f "$0")
HERE=${SELF%/*}

# Add our lib directory to the path
export PATH="${HERE}/usr/bin:${PATH}"

# Install OBS plugin if not already installed
OBS_PLUGIN_DIR="${HOME}/.config/obs-studio/plugins/obs-crowdcast/bin/64bit"
if [ ! -f "${OBS_PLUGIN_DIR}/obs-crowdcast.so" ] && [ -f "${HERE}/usr/lib/obs-plugins/obs-crowdcast.so" ]; then
    mkdir -p "${OBS_PLUGIN_DIR}"
    cp "${HERE}/usr/lib/obs-plugins/obs-crowdcast.so" "${OBS_PLUGIN_DIR}/"
    
    LOCALE_DIR="${HOME}/.config/obs-studio/plugins/obs-crowdcast/data/locale"
    mkdir -p "${LOCALE_DIR}"
    cp "${HERE}/usr/share/obs/obs-plugins/obs-crowdcast/locale/"* "${LOCALE_DIR}/" 2>/dev/null || true
fi

exec "${HERE}/usr/bin/crowdcast-agent" "$@"
EOF
chmod +x "$APPDIR/AppRun"

# Download appimagetool if not present
APPIMAGETOOL="$BUILD_DIR/appimagetool"
if [ ! -f "$APPIMAGETOOL" ]; then
    echo "Downloading appimagetool..."
    wget -q "https://github.com/AppImage/AppImageKit/releases/download/continuous/appimagetool-x86_64.AppImage" -O "$APPIMAGETOOL"
    chmod +x "$APPIMAGETOOL"
fi

# Build the AppImage
echo "Creating AppImage..."
cd "$BUILD_DIR"
ARCH=x86_64 "$APPIMAGETOOL" "$APPDIR" "CrowdCast-x86_64.AppImage"

echo "Done! AppImage created at: $BUILD_DIR/CrowdCast-x86_64.AppImage"
