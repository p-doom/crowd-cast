# CrowdCast

A cross-platform infrastructure for capturing paired screencast and keyboard/mouse input data.

## Overview

CrowdCast consists of two components:

1. **OBS Plugin** (`obs-crowdcast-plugin/`) - A C plugin for OBS Studio that exposes window capture state and provides window enumeration via obs-websocket vendor requests
2. **Agent** (`agent/`) - A Rust application that coordinates with OBS, captures input events, and uploads paired data to S3

## Architecture

```
┌────────────────────────────────────────────────────────────────┐
│                        CrowdCast Agent (Rust)                  │
│  ┌──────────┐  ┌───────────────┐  ┌──────────┐  ┌───────────┐  │
│  │ Tray UI  │  │ OBS Controller│  │  Sync    │  │ Uploader  │  │
│  └──────────┘  └───────┬───────┘  │  Engine  │  └─────┬─────┘  │
│                        │          └────┬─────┘        │        │
│                        │               │              │        │
│                   obs-websocket        │         pre-signed    │
│                        │          ┌────┴─────┐       URLs      │
│                        │          │  rdev/   │        │        │
│                        │          │  evdev   │        │        │
└────────────────────────┼──────────┴──────────┴────────┼────────┘
                         │                              │
                         ▼                              ▼
┌─────────────────────────────────────┐        ┌───────────────┐
│           OBS Studio                │        │  Lambda + S3  │
│  ┌───────────────────────────────┐  │        └───────────────┘
│  │  CrowdCast Plugin (C)         │  │
│  │  - Tracks hooked state        │  │
│  │  - Window enumeration         │  │
│  │  - Source creation            │  │
│  └───────────────────────────────┘  │
└─────────────────────────────────────┘
```

## Features

- **Privacy-aware capture**: Only logs input when OBS is actively capturing a window (not during blackscreen)
- **Cross-platform**: Windows, macOS, Linux (X11 and Wayland best-effort)
- **Hardware acceleration**: Uses OBS's native encoding (NVENC, VAAPI, QSV, VideoToolbox)
- **Efficient uploads**: Chunked uploads via pre-signed S3 URLs
- **Minimal storage burden**: Upload and delete locally or stream to YouTube
- **Plug-and-play installation**: Setup wizard handles OBS detection, plugin installation, profile configuration, and permissions

## Quick Start

### Prerequisites

- [OBS Studio](https://obsproject.com/) 28.0 or later
- Rust toolchain (for building the agent)

### Installation

```bash
# Build the agent
cd agent
cargo build --release

# Run the setup wizard
./target/release/crowdcast-agent --setup
```

The setup wizard will:
1. Detect or prompt you to install OBS Studio
2. **Automatically download and install** the CrowdCast OBS plugin
3. Create an optimized OBS profile with hardware encoding
4. Launch OBS (so the plugin loads)
5. Let you select which applications to capture (browsers, IDEs, etc.)
6. Request necessary OS permissions (Accessibility, Screen Recording)
7. Optionally configure autostart

After setup, simply run `crowdcast-agent` and it will automatically:
- Launch OBS minimized to the system tray
- Start capturing input when you begin recording
- Upload paired data to your configured endpoint

## Platform-Specific Setup

### macOS

1. Grant **Accessibility** permission to the agent (System Settings → Privacy & Security → Accessibility)
2. Grant **Screen Recording** permission if using screenshot-based features

### Linux (Wayland)

For Wayland support, the agent uses `evdev` which requires the user to be in the `input` group:

```bash
sudo usermod -aG input $USER
# Log out and back in
```

### Windows

No special setup required. Run as administrator if input capture doesn't work.

## Configuration

Configuration is stored at:
- Linux: `~/.config/crowdcast/agent/config.toml`
- macOS: `~/Library/Application Support/dev.crowdcast.agent/config.toml`
- Windows: `%APPDATA%\crowdcast\agent\config\config.toml`

Example configuration:

```toml
[obs]
host = "localhost"
port = 4455
password = "your-password"  # Optional
poll_interval_ms = 150

[input]
capture_keyboard = true
capture_mouse_move = true
capture_mouse_click = true
capture_mouse_scroll = true

[upload]
lambda_endpoint = "https://your-api.amazonaws.com/presign"
delete_after_upload = true
```

## Usage

### First Run (Recommended)

```bash
crowdcast-agent --setup
```

This runs the interactive setup wizard that guides you through configuration.

### Normal Usage

```bash
crowdcast-agent
```

The agent will:
1. Launch OBS minimized (if not already running)
2. Connect via WebSocket
3. Show in your system tray
4. Capture input when OBS is recording

### Command Line Options

```
crowdcast-agent [OPTIONS]

OPTIONS:
    -h, --help            Print help message
    -s, --setup           Run the setup wizard
    --non-interactive     Run setup without prompts (use defaults)
```

### Application Selection

During setup, you'll be prompted to select which applications to capture:

```
Step 5/7: Selecting applications to capture...
  Found 8 windows (3 suggested)

Suggested applications:
  [x] 1. Firefox (Mozilla Firefox - Google Search)
  [x] 2. Cursor (main.rs - crowd-cast)
  [ ] 3. Preview (document.pdf)

Other open windows:
  [ ] 4. Slack (general - Slack)
  [ ] 5. Discord

> 3
  [x] 3. Preview (document.pdf)

> [Enter]
Creating 3 capture sources...
  [✓] Created 3 window capture sources
```

Suggested apps (browsers, IDEs, PDF viewers, terminals) are pre-selected. Toggle with numbers, or use `a` for all suggested, `n` for none.

### Input Capture Behavior

Input capture automatically:
- **Starts** when OBS is recording/streaming AND at least one window capture source is hooked
- **Pauses** when no window capture sources are hooked (e.g., captured app not in foreground)
- **Stops** when OBS stops recording/streaming

## Data Format

Input logs are stored in MessagePack format with the following structure:

```json
{
  "session_id": "uuid",
  "chunk_id": "0",
  "start_time_us": 1234567890,
  "end_time_us": 1234567899,
  "events": [
    {
      "timestamp_us": 1234567890,
      "event": {
        "type": "KeyPress",
        "data": { "code": 64, "name": "KeyA" }
      }
    }
  ],
  "metadata": {
    "obs_scene": "Main Scene",
    "pause_count": 0,
    "pause_duration_us": 0,
    "agent_version": "0.1.0",
    "platform": "macos"
  }
}
```

## Backend Setup

The agent expects a Lambda endpoint that returns pre-signed S3 URLs. Example Lambda handler:

```python
import boto3
import json

s3 = boto3.client('s3')
BUCKET = 'your-bucket'

def handler(event, context):
    body = json.loads(event['body'])
    session_id = body['session_id']
    chunk_id = body['chunk_id']
    
    video_key = f"sessions/{session_id}/{chunk_id}.mp4"
    input_key = f"sessions/{session_id}/{chunk_id}.msgpack"
    
    video_url = s3.generate_presigned_url(
        'put_object',
        Params={'Bucket': BUCKET, 'Key': video_key, 'ContentType': 'video/mp4'},
        ExpiresIn=3600
    )
    
    input_url = s3.generate_presigned_url(
        'put_object',
        Params={'Bucket': BUCKET, 'Key': input_key, 'ContentType': 'application/msgpack'},
        ExpiresIn=3600
    )
    
    return {
        'statusCode': 200,
        'body': json.dumps({'video_url': video_url, 'input_url': input_url})
    }
```

## Development

This section is for contributors who want to modify CrowdCast.

### Building the OBS Plugin

The plugin is automatically built by CI and distributed via GitHub Releases. If you need to build it locally for development:

**Note:** The plugin uses the official [obs-websocket vendor API](https://github.com/obsproject/obs-websocket/blob/master/lib/obs-websocket-api.h). The header is included in `deps/obs-websocket-api/`.

#### Linux

```bash
# Install OBS development package
sudo apt install obs-studio libobs-dev  # Ubuntu/Debian
sudo dnf install obs-studio obs-studio-devel  # Fedora

# Build
cd obs-crowdcast-plugin
cmake -B build -DCMAKE_BUILD_TYPE=Release
cmake --build build
```

#### macOS

macOS plugins require a `.plugin` bundle format. The CI workflow produces this automatically.

```bash
# Clone OBS source (for headers only)
git clone --depth 1 --branch 31.0.0 https://github.com/obsproject/obs-studio.git obs-source

# Build (requires OBS.app installed)
cd obs-crowdcast-plugin
cmake -B build -DCMAKE_BUILD_TYPE=Release \
  -DOBS_SOURCE_DIR="../obs-source"
cmake --build build

# Create the .plugin bundle
mkdir -p build/obs-crowdcast.plugin/Contents/{MacOS,Resources/locale}
cp build/obs-crowdcast.so build/obs-crowdcast.plugin/Contents/MacOS/obs-crowdcast
cp data/locale/*.ini build/obs-crowdcast.plugin/Contents/Resources/locale/
cat > build/obs-crowdcast.plugin/Contents/Info.plist << 'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key>
  <string>obs-crowdcast</string>
  <key>CFBundleIdentifier</key>
  <string>com.crowdcast.obs-plugin</string>
  <key>CFBundleName</key>
  <string>obs-crowdcast</string>
  <key>CFBundlePackageType</key>
  <string>BNDL</string>
  <key>CFBundleVersion</key>
  <string>1</string>
</dict>
</plist>
EOF

# Install to OBS plugins directory
cp -r build/obs-crowdcast.plugin ~/Library/Application\ Support/obs-studio/plugins/
```

#### Windows

```bash
# Clone OBS source
git clone --depth 1 --branch 31.0.0 https://github.com/obsproject/obs-studio.git ../obs-source

# Download pre-built OBS
# Extract to ../obs-installed

cd obs-crowdcast-plugin
cmake -B build -G "Visual Studio 17 2022" ^
  -DOBS_SOURCE_DIR="../obs-source" ^
  -DOBS_INSTALLED_DIR="../obs-installed"
cmake --build build --config Release
```

### Building Installers

#### Windows (NSIS)

```bash
cd agent && cargo build --release
makensis installer/windows/crowdcast.nsi
```

#### macOS (DMG)

```bash
./installer/macos/build-dmg.sh
```

#### Linux (AppImage)

```bash
./installer/linux/build-appimage.sh
```

### CI/CD

The OBS plugin is built automatically for all platforms via GitHub Actions. Tagged releases (e.g., `v1.0.0`) trigger artifact uploads to GitHub Releases.

- **Linux**: `.so` file
- **macOS**: `.zip` containing a `.plugin` bundle (required format for macOS OBS)
- **Windows**: `.dll` file

The agent's setup wizard will automatically download and install the appropriate plugin for the user's platform. On macOS, this includes extracting the `.plugin` bundle to `~/Library/Application Support/obs-studio/plugins/`.

## License

- OBS Plugin: GPL-2.0 (required for OBS plugins)
- Agent: MIT

## Contributing

Contributions welcome! Please open an issue first to discuss proposed changes.
