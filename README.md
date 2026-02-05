<div align="center">
  <img src="https://github.com/p-doom/crowd-code/blob/main/img/pdoom-logo.png?raw=true" width="60%" alt="p(doom)" />
</div>
<hr>
<div align="center" style="line-height: 1;">
  <a href="https://www.pdoom.org/"><img alt="Homepage"
    src="https://img.shields.io/badge/Homepage-p%28doom%29-white?logo=home&logoColor=black"/></a>
  <a href="https://huggingface.co/p-doom"><img alt="Hugging Face"
    src="https://img.shields.io/badge/%F0%9F%A4%97%20Hugging%20Face-p--doom-ffc107?color=ffc107&logoColor=white"/></a>
  <br>
  <a href="https://discord.gg/G4JNuPX2VR"><img alt="Discord"
    src="https://img.shields.io/badge/Discord-p%28doom%29-7289da?logo=discord&logoColor=white&color=7289da"/></a>
  <a href="https://github.com/p-doom"><img alt="GitHub"
    src="https://img.shields.io/badge/GitHub-p--doom-24292e?logo=github&logoColor=white"/></a>
  <a href="https://twitter.com/prob_doom"><img alt="Twitter Follow"
    src="https://img.shields.io/badge/Twitter-prob__doom-white?logo=x&logoColor=white"/></a>
  <br>
  <a href="LICENSE.md" style="margin: 2px;">
    <img alt="License" src="https://img.shields.io/badge/License-MIT-f5de53?&color=f5de53" style="display: inline-block; vertical-align: middle;"/>
  </a>
  <br>
</div>

# `crowd-cast`:  Crowd-Sourcing Months-Long Trajectories of Human Computer Work 

Cross-platform infrastructure for capturing paired screencast and keyboard/mouse input data.

## Overview

crowd-cast is a single-binary agent that embeds [libobs](https://github.com/obsproject/obs-studio) for screen capture and recording, eliminating the need to install OBS Studio separately.

**Key components:**
- **Embedded libobs** - Screen/window capture with hardware encoding (via [libobs-rs](https://github.com/joshprk/libobs-rs))
- **Sync Engine** - Coordinates recording with input capture, filters by frontmost app
- **Input Capture** - Cross-platform keyboard/mouse capture (rdev/evdev)
- **System Tray** - Control recording from the menu bar

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     crowd-cast Agent (Rust)                     │
│  ┌──────────┐  ┌────────────────┐  ┌─────────────────────────┐  │
│  │ Tray UI  │  │ Embedded libobs│  │      Sync Engine        │  │
│  │          │  │ (libobs-rs)    │  │  - Frontmost app detect │  │
│  └──────────┘  │                │  │  - Input filtering      │  │
│                │  ┌───────────┐ │  │  - Event buffering      │  │
│                │  │mac-capture│ │  └───────────┬─────────────┘  │
│                │  │obs-x264   │ │              │                │
│                │  │obs-ffmpeg │ │        ┌─────┴─────┐          │
│                │  └───────────┘ │        │ rdev/evdev│          │
│                └────────────────┘        └───────────┘          │
│                        │                       │                │
│                   Video Output            Input Events          │
│                        │                       │                │
│                        └───────────┬───────────┘                │
│                                    │                            │
│                              ┌─────┴─────┐                      │
│                              │ Uploader  │                      │
│                              └─────┬─────┘                      │
└────────────────────────────────────┼────────────────────────────┘
                                     │
                                     ▼
                              ┌─────────────┐
                              │ Lambda + S3 │
                              └─────────────┘
```

## Features

- **Single binary**: No external OBS installation required - libobs is embedded
- **Privacy-aware capture**: Only logs input when selected applications are in the foreground
- **Cross-platform**: Windows, macOS, Linux (X11 and Wayland best-effort)
- **Hardware acceleration**: Uses libobs native encoding (NVENC, VAAPI, QSV, VideoToolbox)
- **Efficient uploads**: Chunked uploads via pre-signed S3 URLs
- **Easy setup**: Wizard handles permissions and application selection

## Quick Start

### Prerequisites

- Rust toolchain (for building from source)
- macOS: Homebrew with `brew install simde` (for ARM builds)

### Installation

```bash
# Install cargo-obs-build tool
cargo install cargo-obs-build

# Clone the repository
git clone https://github.com/p-doom/crowd-cast.git
cd crowd-cast

# Download OBS binaries (required for linking)
cargo obs-build build --out-dir target/release

# Build the agent
cargo build --release

# Run the setup wizard
./target/release/crowd-cast-agent --setup
```

The setup wizard will:
1. Check and request OS permissions (Accessibility, Screen Recording)
2. Let you select which applications to capture (browsers, IDEs, etc.)
3. Optionally configure autostart

After setup, simply run `crowd-cast-agent` and it will:
- Show in the system tray
- Automatically download OBS libraries if needed (via libobs-bootstrapper)
- Capture input only when selected apps are in the foreground
- Upload paired video + input data to your configured endpoint

## Platform-Specific Setup

### macOS

1. Grant **Accessibility** permission to the agent (System Settings → Privacy & Security → Accessibility)

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
- Linux: `~/.config/crowd-cast/agent/config.toml`
- macOS: `~/Library/Application Support/dev.crowd-cast.agent/config.toml`
- Windows: `%APPDATA%\crowd-cast\agent\config\config.toml`

Example configuration:

```toml
[capture]
# Apps to capture (bundle IDs on macOS, process names on Linux/Windows)
target_apps = ["com.apple.Safari", "com.microsoft.VSCode", "com.todesktop.230313mzl4w4u92"]
capture_all = false  # Set true to capture all apps
poll_interval_ms = 100  # Frontmost app detection interval

[input]
capture_keyboard = true
capture_mouse_move = true
capture_mouse_click = true
capture_mouse_scroll = true

[upload]
delete_after_upload = true

[recording]
output_directory = "/tmp/crowd-cast-recordings"
autostart_on_launch = true
```

Upload endpoint configuration is provided at build time via
`CROWD_CAST_API_GATEWAY_URL`.

## Usage

### First Run (Recommended)

```bash
crowd-cast-agent --setup
```

This runs the interactive setup wizard that guides you through configuration.

### Normal Usage

```bash
crowd-cast-agent
```

The agent will:
1. Bootstrap OBS libraries (downloads if needed)
2. Initialize embedded libobs for capture
3. Show in your system tray
4. Capture input when selected apps are in foreground

### Command Line Options

```
crowd-cast-agent [OPTIONS]

OPTIONS:
    -h, --help    Print help message
    -s, --setup   Run the setup wizard (re-select apps, etc.)

ENVIRONMENT:
    RUST_LOG      Set log level (e.g., debug, info, warn)
    CROWD_CAST_LOG_PATH
                  Override log directory (default: ~/Library/Logs/crowd-cast on macOS)
    CROWD_CAST_API_GATEWAY_URL
                  Lambda endpoint for pre-signed S3 URLs (set at build time)
```

### Application Selection

During setup, you'll be prompted to select which applications to capture:

```
Step 2: Select applications to capture

Input will only be captured when one of the selected
applications is in the foreground.

Capture input for ALL applications? [y/N]: n

Available applications:

    1. Cursor (Cursor)
    2. Discord (Discord)
    3. Firefox (firefox)
    4. Google Chrome (Google Chrome)
    5. Slack (Slack)
    6. Terminal (Terminal)

Enter application numbers to select (comma-separated)
Example: 1,3,5 or 'all' for all apps, 'none' to skip

Selection: 1,3,4
  Selected: Cursor (Cursor)
  Selected: Firefox (firefox)
  Selected: Google Chrome (Google Chrome)
```

### Input Capture Behavior

Input capture automatically:
- **Enabled** when a selected application is the frontmost (active) window
- **Disabled** when a non-selected application is in foreground
- **Synced** with video recording timestamps for perfect alignment

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

## Utilities

Overlay keylogs on top of a screen capture:

```bash
python scripts/overlay_keylogs.py --video capture.mp4 --input input.msgpack --output capture_with_keys.mp4
```

To just generate subtitles (ASS):

```bash
python scripts/overlay_keylogs.py --input input.msgpack --ass-out keylogs.ass
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
    file_name = body['fileName']
    version = body['version']
    user_id = body['userId']
    
    key = f"uploads/{version}/{user_id}/{file_name}"
    
    content_type = (
        "application/msgpack" if file_name.endswith(".msgpack") else "video/mp4"
    )

    upload_url = s3.generate_presigned_url(
        'put_object',
        Params={'Bucket': BUCKET, 'Key': key, 'ContentType': content_type},
        ExpiresIn=3600
    )
    
    return {
        'statusCode': 200,
        'body': json.dumps({
            'uploadUrl': upload_url,
            'key': key,
            'contentType': content_type,
        })
    }
```

## Development

This section is for contributors who want to modify crowd-cast.

### Project Structure

```
crowd-cast/
├── src/
│   ├── capture/       # libobs integration, frontmost app detection
│   ├── input/         # Keyboard/mouse capture backends
│   ├── sync/          # Sync engine coordinating capture + input
│   ├── installer/     # Setup wizard, permissions
│   └── ui/            # System tray
├── Cargo.toml
├── libobs-rs/             # Fork of libobs-rs with macOS support
└── scripts/               # Utility scripts
```

### Building from Source

#### Prerequisites

**macOS (Apple Silicon):**
```bash
brew install simde  # Required for ARM builds
```

**Linux:**
```bash
# Ubuntu/Debian
sudo apt install libgtk-3-dev libayatana-appindicator3-dev

# Fedora
sudo dnf install gtk3-devel libappindicator-gtk3-devel
```

#### Build Steps

```bash
# 1. Clone with submodules
git clone --recursive https://github.com/p-doom/crowd-cast.git
cd crowd-cast

# 2. Download OBS binaries for linking
cargo build --release --package cargo-obs-build
./target/release/cargo-obs-build build --out-dir target/debug

# 3. Build the agent
cargo build

# 4. Run tests
cargo test
```

### libobs-rs Integration

The agent uses [libobs-rs](https://github.com/joshprk/libobs-rs) to embed OBS functionality. Key crates:

- `libobs` - Raw FFI bindings to libobs
- `libobs-wrapper` - Safe Rust wrapper
- `libobs-bootstrapper` - Downloads OBS binaries at runtime (macOS)
- `cargo-obs-build` - Downloads OBS binaries at build time

The fork at `libobs-rs/` includes macOS support from [PR #53](https://github.com/joshprk/libobs-rs/pull/53).

### Adding New Capture Sources

To add support for new capture types, implement them in `src/capture/sources.rs`:

```rust
pub fn new_window_capture(ctx: &ObsContext, window_name: &str) -> Result<ObsSourceRef> {
    // Use libobs-wrapper to create window capture source
}
```

## License

MIT License, see [LICENSE.md](LICENSE.md)

## Contributing

Contributions welcome! Please open an issue first to discuss proposed changes.
